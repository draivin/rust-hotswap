#![feature(quote, plugin_registrar, rustc_private, box_syntax, stmt_expr_attributes)]

extern crate syntax;
extern crate rustc_plugin;
extern crate aster;

#[macro_use]
extern crate lazy_static;

use rustc_plugin::registry::Registry;

use syntax::abi::Abi;
use syntax::ast::{Attribute, Block, Expr, FnDecl, FunctionRetTy, Ident, Item, ItemKind, MetaItem,
                  MetaItemKind, Mod, Mutability, PatKind, Stmt, TokenTree, Ty, Visibility};
use syntax::codemap::{self, Span};
use syntax::ext::base::{Annotatable, ExtCtxt, MacEager, MacResult};
use syntax::ext::base::SyntaxExtension::MultiModifier;
use syntax::ext::build::AstBuilder;
use syntax::ext::quote::rt::ToTokens;
use syntax::feature_gate::AttributeType;
use syntax::parse::token::{self, intern};
use syntax::ptr::P;

use std::collections::HashSet;
use std::sync::RwLock;
use std::mem;


type HotswapData = Vec<String>;

lazy_static! {
    static ref HOTSWAP_DATA: RwLock<HotswapData> = RwLock::new(Vec::new());
}

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
    reg.register_syntax_extension(intern("hotswap_header"), MultiModifier(box expand_header));
    reg.register_macro("hotswap_start", expand_macro);
    reg.register_attribute("hotswap".to_string(), AttributeType::Whitelisted);
}

fn expand_header(cx: &mut ExtCtxt, _: Span, _: &MetaItem, annotatable: Annotatable) -> Annotatable {
    if let Annotatable::Item(item) = annotatable {
        if let &ItemKind::Mod(ref m) = &item.node {
            let mut hotswap_data = &mut *HOTSWAP_DATA.write().unwrap();
            let new_mod_items = match crate_type().as_ref() {
                "bin" => expand_bin_mod(cx, m, &mut hotswap_data),
                _ => expand_lib_mod(cx, m, &mut hotswap_data),
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

fn expand_macro(cx: &mut ExtCtxt, _: Span, tt: &[TokenTree]) -> Box<MacResult> {
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

    let builder = aster::AstBuilder::new();
    let names = &*HOTSWAP_DATA.read().unwrap();

    // Create one statement per hotswapped function, each
    // statement will update its global variable to point
    // to the latest dynamic address.
    let id_names: Vec<(P<Expr>, &str)> = names.iter()
        .map(|name| (builder.expr().id(global_fn_name(name)), name.as_str()))
        .collect();

    let ref_updaters: Vec<Stmt> = id_names.into_iter()
        .map(|(id, name)| {
            quote_stmt!(cx, {
            $id = *(lib.get::<extern "Rust" fn()>($name.as_bytes())
                .expect("Failed to load Symbol")
                .deref()) as *const () as usize;
        })
                .unwrap()
        })
        .collect();


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

// Reads the build type(lib or bin) from the args.
fn rustc_arg(arg_name: &str) -> String {
    let mut args = std::env::args();
    loop {
        match args.next() {
            Some(arg) => {
                if arg == arg_name {
                    return args.next().unwrap();
                }
            }
            None => panic!("could not find arg"),
        }
    }
}

fn crate_type() -> String {
    rustc_arg("--crate-type")
}

fn crate_name() -> String {
    rustc_arg("--crate-name")
}

// Ignore dead code in the lib build, probably there will be a lot of it
// starting at the `main` function.
fn expand_lib_attrs(cx: &mut ExtCtxt, attrs: &Vec<Attribute>) -> Vec<Attribute> {
    let mut new_attrs = attrs.clone();
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
    let builder = aster::AstBuilder::new();
    new_items.push(builder.item().extern_crate("libloading").build());

    let mut hotswappable_fns = Vec::new();

    for item in &m.items {
        let attr_names = item_attr_names(&item);

        let item = if attr_names.contains("hotswap") {
            hotswappable_fns.push(item_name(&item));
            expand_bin_fn(cx, item)
        } else {
            item.clone()
        };

        new_items.push(item);
    }

    // Create one global variable for each hotswapped function.
    for fn_name in hotswappable_fns {
        hotswap_data.push(fn_name.clone());

        let global_name = global_fn_name(&fn_name);
        let stmt = builder.item().build_item_kind(global_name,
                                                  ItemKind::Static(builder.ty().usize(),
                                                                   Mutability::Mutable,
                                                                   builder.expr().usize(0)));

        new_items.push(stmt);
    }

    new_items
}

fn expand_bin_fn(cx: &mut ExtCtxt, item: &Item) -> P<Item> {
    let mut new_item = item.clone();

    if let &mut ItemKind::Fn(ref fn_decl, _, _, _, _, ref mut block) = &mut new_item.node {
        let new_block = expand_bin_fn_body(cx, fn_decl, &item_name(item));
        mem::replace(block, new_block);
    } else {
        // TODO: write proper warning.
        println!("warning: hotswap only works on functions");
    }

    P(new_item)
}

fn expand_bin_fn_body(cx: &mut ExtCtxt, fn_decl: &FnDecl, fn_name: &str) -> P<Block> {
    let arg_idents = comma_separated_tokens(cx, &arg_idents(fn_decl));
    let arg_types = comma_separated_tokens(cx, &arg_types(fn_decl));
    let ret = return_type(cx, fn_decl);

    let builder = aster::AstBuilder::new();
    let global_name = builder.expr().id(global_fn_name(fn_name));

    P(quote_block!(cx, {
        let func = unsafe {
            use std::mem::transmute;
            transmute::<_, extern "Rust" fn($arg_types) -> $ret>($global_name as *const ())
        };

        func($arg_idents)
    })
        .unwrap())
}

fn global_fn_name(fn_name: &str) -> String {
    "_HOTSWAP_".to_string() + fn_name
}

fn comma_separated_tokens<T: ToTokens>(cx: &mut ExtCtxt, entries: &[T]) -> Vec<TokenTree> {
    entries.iter()
        .map(|t| t.to_tokens(cx))
        .collect::<Vec<_>>()
        .join(&TokenTree::Token(codemap::DUMMY_SP, token::Comma))
}

fn arg_idents(decl: &FnDecl) -> Vec<Ident> {
    decl.inputs
        .iter()
        .filter_map(|arg| {
            let mut ident = None;
            arg.pat.walk(&mut |pat| {
                if let &PatKind::Ident(_, ref span_ident, _) = &pat.node {
                    ident = Some(span_ident.node.clone());
                    false
                } else {
                    true
                }
            });
            ident
        })
        .collect()
}

fn arg_types(fn_decl: &FnDecl) -> Vec<Ty> {
    fn_decl.inputs.iter().map(|arg| (*arg.ty).clone()).collect()
}

fn return_type(cx: &mut ExtCtxt, fn_decl: &FnDecl) -> P<Ty> {
    match &fn_decl.output {
        &FunctionRetTy::Ty(ref ty) => ty.clone(),
        _ => quote_ty!(cx, ()),
    }
}

fn item_name(item: &Item) -> String {
    format!("{}", item.ident.name)
}

fn item_attr_names(item: &Item) -> HashSet<&str> {
    let mut attr_names = HashSet::new();

    for attr in &item.attrs {
        if let &MetaItemKind::Word(ref word) = &attr.node.value.node {
            attr_names.insert(&**word);
        }
    }

    attr_names
}
