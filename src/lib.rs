#![feature(quote, plugin_registrar, rustc_private, box_syntax, stmt_expr_attributes)]

extern crate syntax;
extern crate rustc_plugin;

use rustc_plugin::registry::Registry;

use syntax::abi::Abi;
use syntax::ast::{Attribute, Ident, Item, ItemKind, MetaItem, Mod, Ty, Visibility, Name};
use syntax::attr;
use syntax::codemap::Span;
use syntax::ext::base::{Annotatable, ExtCtxt, TTMacroExpander, MacEager, MacResult,
                        MultiItemModifier};
use syntax::ext::base::SyntaxExtension::{MultiModifier, NormalTT};
use syntax::feature_gate::AttributeType;
use syntax::ptr::P;
use syntax::tokenstream::TokenStream;

use std::cell::RefCell;
use std::rc::Rc;

use std::mem;

mod codegen;
mod util;

use util::syntax::get_fn_info;
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
pub fn plugin_registrar(reg: &mut Registry) {
    let fn_list = Rc::new(RefCell::new(Vec::new()));

    let header_extension = HotswapHeaderExtension { fn_list: Rc::clone(&fn_list) };
    let macro_extension = HotswapMacroExtension { fn_list: Rc::clone(&fn_list) };

    reg.register_syntax_extension(
        Name::intern("hotswap_header"),
        MultiModifier(box header_extension),
    );
    reg.register_syntax_extension(
        Name::intern("hotswap_start"),
        NormalTT {
            expander: box macro_extension,
            def_info: None,
            allow_internal_unsafe: false,
            allow_internal_unstable: false,
        },
    );

    reg.register_attribute("hotswap".to_string(), AttributeType::Whitelisted);
}


pub struct HotswapFnInfo {
    name: String,
    input_types: Vec<Ty>,
    input_idents: Vec<Ident>,
    output_type: P<Ty>,
}

type HotswapFnList = Vec<HotswapFnInfo>;

struct HotswapHeaderExtension {
    fn_list: Rc<RefCell<HotswapFnList>>,
}

struct HotswapMacroExtension {
    fn_list: Rc<RefCell<HotswapFnList>>,
}

impl MultiItemModifier for HotswapHeaderExtension {
    fn expand(
        &self,
        cx: &mut ExtCtxt,
        _: Span,
        _: &MetaItem,
        annotatable: Annotatable,
    ) -> Vec<Annotatable> {
        let annotatable = if let Annotatable::Item(item) = annotatable {
            let mut item = item.unwrap();
            if let ItemKind::Mod(m) = item.node {
                let mut hotswap_fns = self.fn_list.borrow_mut();

                item.node = ItemKind::Mod(match crate_type().as_ref() {
                    "bin" => {
                        let tmp = expand_bin_mod(cx, m, &mut hotswap_fns);
                        expand_bin_footer(cx, tmp, &mut hotswap_fns)
                    }
                    "dylib" => expand_lib_mod(cx, m),
                    _ => {
                        unimplemented!()
                    }
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
        };

        vec![annotatable]
    }
}

impl TTMacroExpander for HotswapMacroExtension {
    fn expand(&self, cx: &mut ExtCtxt, _: Span, tt: TokenStream) -> Box<MacResult> {
        let hotswap_fns = self.fn_list.borrow();

        // Macro in lib build shouldn't be expanded, as the
        // crate dependencies aren't imported, the length is
        // also 0 when there is no hotswap_header.
        if hotswap_fns.len() == 0 {
            // Just some arbitrary unsafe code that does nothing so the
            // compiler doesn't complain about unnused unsafe blocks.
            //
            // Also happens to stop the build immediately on the lib
            // build if the user haven't wrapped the macro in unsafe,
            // instead of building the lib and stopping on the bin build.
            return MacEager::expr(quote_expr!(cx, {
                &*(0 as *const usize);
            }));
        }

        if !tt.is_empty() {
            // TODO: proper warning when user doesn't leave the macro
            // empty.
            unimplemented!();
        }

        MacEager::expr(codegen::macro_expansion(cx, &hotswap_fns))
    }
}

// Ignore dead code in the lib build, probably there will be a lot of it,
// including the `main` function.
fn expand_lib_attrs(cx: &mut ExtCtxt, mut attrs: Vec<Attribute>) -> Vec<Attribute> {
    attrs.insert(0, quote_attr!(cx, #![allow(unused_imports)]));
    attrs.insert(0, quote_attr!(cx, #![allow(dead_code)]));
    attrs.insert(0, quote_attr!(cx, #![allow(unused_features)]));
    attrs
}

// The lib code marks the hotswapped functions as `no_mangle` and
// exports them.
fn expand_lib_mod(cx: &mut ExtCtxt, mut m: Mod) -> Mod {
    m.items = m.items
        .into_iter()
        .map(|item| {
            let mut item = item.unwrap();

            item.node = match item.node {
                ItemKind::Mod(m) => {
                    // Only functions in public mods can be exported.
                    item.vis = Visibility::Public;
                    ItemKind::Mod(expand_lib_mod(cx, m))
                }
                _ => item.node,
            };

            if attr::contains_name(&item.attrs, "hotswap") {
                P(expand_lib_fn(cx, item))
            } else {
                P(item)
            }
        })
        .collect();

    m
}

fn expand_lib_fn(cx: &mut ExtCtxt, mut item: Item) -> Item {
    if let ItemKind::Fn(_, _, _, ref mut abi, _, _) = item.node {
        // Make lib functions extern and no mangle so they can
        // be imported from the runtime.
        item.attrs.push(quote_attr!(cx, #![no_mangle]));
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
    m.items = m.items
        .into_iter()
        .map(|item| {
            let mut item = item.unwrap();

            item.node = match item.node {
                ItemKind::Mod(m) => {
                    item.vis = Visibility::Public;
                    ItemKind::Mod(expand_bin_mod(cx, m, hotswap_fns))
                }
                _ => item.node,
            };

            if attr::contains_name(&item.attrs, "hotswap") {
                P(expand_bin_fn(cx, item, hotswap_fns))
            } else {
                P(item)
            }
        })
        .collect();

    m
}

fn expand_bin_footer(cx: &mut ExtCtxt, mut m: Mod, hotswap_fns: &mut HotswapFnList) -> Mod {
    // Add crate containing the external dependencies of the runtime.
    m.items.insert(
        0,
        quote_item!(cx, extern crate hotswap_runtime;)
            .unwrap(),
    );

    // Create the mod where the function pointers are located.
    m.items.push(codegen::runtime_mod(cx, hotswap_fns));

    m
}

fn expand_bin_fn(cx: &mut ExtCtxt, mut item: Item, hotswap_fns: &mut HotswapFnList) -> Item {
    if let ItemKind::Fn(..) = item.node {
        let fn_info = get_fn_info(cx, &item);

        if let ItemKind::Fn(_, _, _, _, _, ref mut block) = item.node {
            mem::replace(block, codegen::fn_body(cx, &fn_info));
        }

        hotswap_fns.push(fn_info);
    } else {
        // TODO: write proper warning.
        println!("warning: hotswap only works on functions");
    }

    item
}
