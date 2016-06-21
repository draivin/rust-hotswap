#![feature(quote, plugin_registrar, rustc_private, box_syntax, stmt_expr_attributes)]

extern crate syntax;
extern crate rustc_plugin;

use rustc_plugin::registry::Registry;

use syntax::abi::Abi;
use syntax::ast::{Attribute, Ident, Item, ItemKind, MetaItem, Mod, TokenTree, Ty, Visibility};
use syntax::attr;
use syntax::codemap::Span;
use syntax::ext::base::{Annotatable, ExtCtxt, TTMacroExpander, MacEager, MacResult, MultiItemModifier};
use syntax::ext::base::SyntaxExtension::{MultiModifier, NormalTT};
use syntax::feature_gate::AttributeType;
use syntax::parse::token::intern;
use syntax::ptr::P;

use std::cell::RefCell;
use std::rc::Rc;

use std::mem;

mod util;
mod runtime;

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


pub struct HotswapFnInfo {
    name: String,
    input_types: Vec<Ty>,
    input_idents: Vec<Ident>,
    output_type: P<Ty>
}

pub type HotswapFnList = Vec<HotswapFnInfo>;

struct HotswapHeaderExtension {
    data: Rc<RefCell<HotswapFnList>>
}

struct HotswapMacroExtension {
    data: Rc<RefCell<HotswapFnList>>
}

impl MultiItemModifier for HotswapHeaderExtension {
    fn expand(&self, cx: &mut ExtCtxt, _: Span, _: &MetaItem,
              annotatable: Annotatable) -> Annotatable {

        if let Annotatable::Item(item) = annotatable {
            let mut item = item.unwrap();
            if let ItemKind::Mod(m) = item.node {
                let mut hotswap_fns = self.data.borrow_mut();

                item.node = ItemKind::Mod(match crate_type().as_ref() {
                    "bin" => {
                        let m = expand_bin_mod(cx, m, &mut hotswap_fns);
                        let m = expand_bin_footer(cx, m, &mut hotswap_fns);
                        m
                    },
                    _ => expand_lib_mod(cx, m),
                });

                item.attrs = match crate_type().as_ref() {
                    "dylib" => expand_lib_attrs(cx, item.attrs),
                    _ => item.attrs,
                };

                Annotatable::Item(P(item))
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
            // Just some arbitrary unsafe code that does nothing so the
            // compiler doesn't complain about unnused unsafe blocks.
            //
            // Also happens to stop the build immediately on the lib
            // build, instead of building the lib and stopping on the
            // bin build.
            return MacEager::expr(quote_expr!(cx, {
                &*(0 as *const usize);
            }));
        }

        // Macro should be empty.
        if tt.len() > 0 {
            // TODO: proper warning when user doesn't leave the macro
            // empty.
            unimplemented!();
        }

        let hotswap_fns = self.data.borrow();
        MacEager::expr(runtime::create_macro_expansion(cx, &hotswap_fns))
    }
}

// Ignore dead code in the lib build, probably there will be a lot of it
// starting at the `main` function.
fn expand_lib_attrs(cx: &mut ExtCtxt, mut attrs: Vec<Attribute>) -> Vec<Attribute> {
    attrs.insert(0, quote_attr!(cx, #![allow(unused_imports)]));
    attrs.insert(0, quote_attr!(cx, #![allow(dead_code)]));
    attrs
}

// The lib code marks the hotswapped functions as `no_mangle` and
// exports them.
fn expand_lib_mod(cx: &mut ExtCtxt, mut m: Mod) -> Mod {
    m.items = m.items.into_iter().map(|item| {
        let mut item = item.unwrap();

        item.node = match item.node {
            ItemKind::Mod(m) => {
                // Only functions in public mods can be exported.
                item.vis = Visibility::Public;
                ItemKind::Mod(expand_lib_mod(cx, m))
            },
            _ => item.node
        };

        if attr::contains_name(&item.attrs, "hotswap") {
            P(expand_lib_fn(cx, item))
        } else {
            P(item)
        }
    }).collect();

    m
}

fn expand_lib_fn(cx: &mut ExtCtxt, mut item: Item) -> Item {
    if let &mut ItemKind::Fn(_, _, _, ref mut abi, _, _) = &mut item.node {
        // Make lib functions extern and no mangle so they can
        // be imported from the runtime.

        let attr = quote_attr!(cx, #![no_mangle]);

        item.attrs.push(attr);
        item.vis = Visibility::Public;

        mem::replace(abi, Abi::Rust);
    } else {
        // TODO: write proper warning.
        println!("warning: hotswap only works on functions");
    }

    item
}

// The bin code imports required crates and rewrites the hotswapped
// functions body so it executes the dynamic library functions instead.
fn expand_bin_mod(cx: &mut ExtCtxt, mut m: Mod, hotswap_fns: &mut HotswapFnList) -> Mod {
    m.items = m.items.into_iter().map(|item| {
        let mut item = item.unwrap();

        item.node = match item.node {
            ItemKind::Mod(m) => {
                item.vis = Visibility::Public;
                ItemKind::Mod(expand_bin_mod(cx, m, hotswap_fns))
            },
            _ => item.node
        };

        if attr::contains_name(&item.attrs, "hotswap") {
            P(expand_bin_fn(cx, item, hotswap_fns))
        } else {
            P(item)
        }
    }).collect();

    m
}

fn expand_bin_footer(cx: &mut ExtCtxt, mut m: Mod, hotswap_fns: &mut HotswapFnList) -> Mod {
    // TODO: look for a way to load the crates that does
    // not require them to be a dependency of the client.
    m.items.insert(0, quote_item!(cx, extern crate libloading;).unwrap());

    // Create the mod where the function pointers are located.
    m.items.push(runtime::create_hotswap_mod(cx, hotswap_fns));

    m
}

fn expand_bin_fn(cx: &mut ExtCtxt, mut item: Item, hotswap_fns: &mut HotswapFnList) -> Item {
    if let ItemKind::Fn(ref fn_decl, _, _, _, _, ref mut block) = item.node {
        let fn_info = HotswapFnInfo {
            name: ident_name(&item.ident),
            input_types: arg_types(fn_decl),
            input_idents: arg_idents(fn_decl),
            output_type: return_type(cx, fn_decl)
        };

        let new_block = runtime::create_fn_body(cx, &fn_info);

        mem::replace(block, new_block);

        hotswap_fns.push(fn_info);
    } else {
        // TODO: write proper warning.
        println!("warning: hotswap only works on functions");
    }

    item
}
