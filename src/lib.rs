#![feature(quote, plugin_registrar, rustc_private, box_syntax, stmt_expr_attributes)]

extern crate syntax;
extern crate rustc_plugin;

use rustc_plugin::registry::Registry;

use syntax::abi::Abi;
use syntax::ast::{Attribute, Block, FnDecl, Ident, Item, ItemKind, MetaItem, Mod, TokenTree, Visibility};
use syntax::codemap::Span;
use syntax::ext::base::{Annotatable, ExtCtxt, TTMacroExpander, MacEager, MacResult, MultiItemModifier};
use syntax::ext::base::SyntaxExtension::{MultiModifier, NormalTT};
use syntax::ext::build::AstBuilder;
use syntax::feature_gate::AttributeType;
use syntax::parse::token::intern;
use syntax::ptr::P;

use std::cell::RefCell;
use std::rc::Rc;

use std::mem;
use std::ops::DerefMut;

mod util;
use util::syntax::*;
use util::rustc::*;

// The plugin works by changing the function code depending on the
// build type(bin or lib), when building a lib, it exports all
// functions that are marked as `hotswap`, when building a bin,
// it completely replaces the function body so that the function
// will instead call a dynamic function that is saved in a global
// variable.

// The user should have a `hotswap_start!` macro before using any
// hotswapped functions, otherwise they will call a null pointer.

#[plugin_registrar]
pub fn registrar(reg: &mut Registry) {
    let data = Rc::new(RefCell::new(Vec::new()));

    let header_extension = HotswapHeaderExtension { data: data.clone() };
    let macro_extension = HotswapMacroExtension { data: data.clone() };

    reg.register_syntax_extension(intern("hotswap_header"),
                                  MultiModifier(box header_extension));
    reg.register_syntax_extension(intern("hotswap_start"),
                                  NormalTT(box macro_extension, None, false));

    reg.register_attribute("hotswap".to_string(), AttributeType::Whitelisted);
}


struct HotswapFn {
    name: String
}

type HotswapData = Vec<HotswapFn>;

struct HotswapHeaderExtension {
    data: Rc<RefCell<HotswapData>>
}

struct HotswapMacroExtension {
    data: Rc<RefCell<HotswapData>>
}

impl MultiItemModifier for HotswapHeaderExtension {
    fn expand(&self, cx: &mut ExtCtxt, _: Span, _: &MetaItem,
              annotatable: Annotatable) -> Annotatable {

        if let Annotatable::Item(item) = annotatable {
            if let &ItemKind::Mod(ref m) = &item.node {
                let mut hotswap_data = self.data.borrow_mut();

                let new_mod_items = match crate_type().as_ref() {
                    "bin" => expand_bin_mod(cx, m, hotswap_data.deref_mut()),
                    _ => expand_lib_mod(cx, m, hotswap_data.deref_mut()),
                };

                let new_attrs = match crate_type().as_ref() {
                    "dylib" => expand_lib_attrs(cx, &item.attrs),
                    _ => item.attrs.clone(),
                };

                Annotatable::Item(cx.item(item.span,
                                          item.ident,
                                          new_attrs,
                                          ItemKind::Mod(Mod {
                                              inner: m.inner,
                                              items: new_mod_items,
                                          })))
            } else {
                // TODO: proper warning when the header annotation is
                // used outside a module.
                unimplemented!();
            }

        } else {
            annotatable
        }
    }
}

impl TTMacroExpander for HotswapMacroExtension {
    fn expand(&self, cx: &mut ExtCtxt, _: Span, tt: &[TokenTree]) -> Box<MacResult> {
        // Macro in lib code shouldn't be expanded, as the
        // crate dependencies aren't imported.
        if crate_type() != "bin" {
            return MacEager::expr(quote_expr!(cx, {}));
        }

        // Macro should be empty.
        if tt.len() > 0 {
            // TODO: proper warning when user doesn't leave the macro
            // empty.
            unimplemented!();
        }

        let hotswap_fns = self.data.borrow();

        // Create one statement per hotswapped function, each
        // statement will update its global variable to point
        // to the latest dynamic address.

        let mut ref_updaters = Vec::new();

        for hotswap_fn in hotswap_fns.iter() {
            let name = &hotswap_fn.name;
            let global_ident = global_fn_ident(name);

            let stmt = quote_stmt!(cx, {
                use std::sync::atomic::Ordering;

                let fn_address = *lib.get::<fn()>($name.as_bytes()).unwrap().deref();
                $global_ident.store(fn_address as usize, Ordering::Relaxed);
            }).unwrap();

            ref_updaters.push(stmt);
        }


        #[cfg(target_os = "windows")]
        let dylib_name = crate_name() + ".dll";

        #[cfg(target_os = "macos")]
        let dylib_name = crate_name() + ".dylib";

        #[cfg(any(target_os = "linux",
                  target_os = "freebsd",
                  target_os = "dragonfly"))]
        let dylib_name = "lib".to_string() + &crate_name() + ".so";


        // This is the code that will be injected on the client,
        // and will try to keep the dynamic library updated.

        // Some of the following could be moved outside the block
        // so it is generated at compile time.
        let block = quote_expr!(cx, {
            use std::ops::Deref;
            use std::thread;
            use std::fs;

            use libloading::Library;

            use std::env::current_exe;

            let exe = current_exe().unwrap();
            let dir = exe.parent().unwrap();

            // TODO: warn if dynamic library was not found.
            let tmp_path = dir.join("hotswap-dylib");
            let dylib_file = dir.join($dylib_name);
            let dylib_move = dylib_file.clone();

            let mut last_modified = fs::metadata(&dylib_file).unwrap().modified().unwrap();

            let reload_dylib = move |dylib_num| {
                // Windows locks the dynamic library once it is loaded, so
                // I'm creating a copy for now.
                let copy_name = format!("{}{}.{}", dylib_move.file_stem().unwrap().to_str().unwrap(),
                                                   dylib_num,
                                                   dylib_move.extension().unwrap().to_str().unwrap());


                let mut dylib_copy = tmp_path.clone();
                fs::create_dir_all(&tmp_path).unwrap();

                dylib_copy.push(copy_name);
                fs::copy(&dylib_move, &dylib_copy).unwrap();

                let lib = Library::new(dylib_copy.to_str().unwrap()).expect("Failed to load library");

                unsafe {
                    $ref_updaters
                };

                // TODO: unload unnused library and delete dynamic library copy.
                // FIXME: leaking memory for now.
                std::mem::forget(lib);
            };

            reload_dylib(0);

            thread::spawn(move || {
                let mut dylib_num = 1;

                loop {
                    thread::sleep(std::time::Duration::from_millis(5000));

                    // TODO: use some filesystem notification crate
                    // so it reloads as soon as the file changes.
                    let modified = match fs::metadata(&dylib_file) {
                        Ok(metadata) => metadata.modified().unwrap(),
                        _ => continue,
                    };

                    if modified > last_modified {
                        last_modified = modified;
                        reload_dylib(dylib_num);
                        dylib_num += 1;
                    }
                }
            });
        })
            .unwrap();

        MacEager::expr(P(block))
    }
}

// Ignore dead code in the lib build, probably there will be a lot of it
// starting at the `main` function.
fn expand_lib_attrs(cx: &mut ExtCtxt, attrs: &Vec<Attribute>) -> Vec<Attribute> {
    let mut new_attrs = attrs.clone();
    new_attrs.insert(0, quote_attr!(cx, #![allow(unused_imports)]));
    new_attrs.insert(0, quote_attr!(cx, #![allow(dead_code)]));
    new_attrs
}

// The lib code marks the hotswapped functions as `no_mangle` and
// exports them.
fn expand_lib_mod(cx: &mut ExtCtxt, m: &Mod, _: &mut HotswapData) -> Vec<P<Item>> {
    let mut new_items = Vec::new();

    for item in &m.items {
        let attr_names = item_attr_names(&item);

        let item = if attr_names.contains("hotswap") {
            expand_lib_fn(cx, item)
        } else {
            item.clone()
        };

        new_items.push(item);
    }

    new_items
}

fn expand_lib_fn(cx: &mut ExtCtxt, item: &Item) -> P<Item> {
    let mut new_item = item.clone();

    if let &mut ItemKind::Fn(_, _, _, ref mut abi, _, _) = &mut new_item.node {
        let attr = quote_attr!(cx, #![no_mangle]);

        new_item.attrs.push(attr);

        mem::replace(abi, Abi::Rust);
        mem::replace(&mut new_item.vis, Visibility::Public);
    } else {
        // TODO: write proper warning.
        println!("warning: hotswap only works on functions");
    }

    P(new_item)
}

// The bin code imports required crates and rewrites the hotswapped
// functions body so it executes the dynamic library functions instead.
fn expand_bin_mod(cx: &mut ExtCtxt, m: &Mod, hotswap_data: &mut HotswapData) -> Vec<P<Item>> {
    let mut new_items = Vec::new();

    // TODO: look for a way to load the crates that does
    // not require them to be a dependency of the client.
    new_items.push(quote_item!(cx, extern crate libloading;).unwrap());

    for item in &m.items {
        let attr_names = item_attr_names(&item);

        let item = if attr_names.contains("hotswap") {
            expand_bin_fn(cx, item, hotswap_data)
        } else {
            item.clone()
        };

        new_items.push(item);
    }


    let atomic_usize = quote_ty!(cx, std::sync::atomic::AtomicUsize);
    let atomic_usize_init = P(quote_expr!(cx, std::sync::atomic::ATOMIC_USIZE_INIT).unwrap());

    // Create one global variable for each hotswapped function.
    for hotswap_fn in hotswap_data {
        let global_name = global_fn_ident(&hotswap_fn.name);
        let item = quote_item!(cx,
            #[allow(non_upper_case_globals)]
            static $global_name: $atomic_usize = $atomic_usize_init;
        ).unwrap();

        new_items.push(item);
    }

    new_items
}

fn expand_bin_fn(cx: &mut ExtCtxt, item: &Item, hotswap_data: &mut HotswapData) -> P<Item> {
    let mut new_item = item.clone();

    if let &mut ItemKind::Fn(ref fn_decl, _, _, _, _, ref mut block) = &mut new_item.node {
        let new_block = expand_bin_fn_body(cx, fn_decl, &item_name(item), hotswap_data);
        mem::replace(block, new_block);
    } else {
        // TODO: write proper warning.
        println!("warning: hotswap only works on functions");
    }

    P(new_item)
}

fn expand_bin_fn_body(cx: &mut ExtCtxt, fn_decl: &FnDecl, fn_name: &str,
                      hotswap_data: &mut HotswapData) -> P<Block> {

    let arg_idents = comma_separated_tokens(cx, &arg_idents(fn_decl));
    let arg_types = comma_separated_tokens(cx, &arg_types(fn_decl));
    let ret = return_type(cx, fn_decl);

    hotswap_data.push(HotswapFn {
        name: fn_name.to_string()
    });

    let global_name = global_fn_ident(fn_name);

    P(quote_block!(cx, {
        let func = unsafe {
            use std::mem::transmute;
            use std::sync::atomic::Ordering;
            transmute::<_, extern "Rust" fn($arg_types) -> $ret>($global_name.load(Ordering::Relaxed))
        };

        func($arg_idents)
    }).unwrap())
}

pub fn global_fn_ident(fn_name: &str) -> Ident {
    Ident::with_empty_ctxt(intern(&("_HOTSWAP_".to_string() + fn_name)))
}
