#![feature(quote, plugin_registrar, rustc_private, box_syntax, stmt_expr_attributes)]

extern crate syntax;
extern crate rustc_plugin;

use rustc_plugin::registry::Registry;

use syntax::abi::Abi;
use syntax::ast::{Attribute, Ident, Item, ItemKind, MetaItem, Mod, TokenTree, Ty, Visibility};
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
            if let &ItemKind::Mod(ref m) = &item.node {
                let mut hotswap_fns = self.data.borrow_mut();

                let new_mod_items = match crate_type().as_ref() {
                    "bin" => expand_bin_mod(cx, m, hotswap_fns.deref_mut()),
                    _ => expand_lib_mod(cx, m),
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
        MacEager::expr(runtime::create_macro_expansion(cx, &hotswap_fns))
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
fn expand_lib_mod(cx: &mut ExtCtxt, m: &Mod) -> Vec<P<Item>> {
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
        // Make lib functions extern and no mangle so they can
        // be imported from the runtime

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
fn expand_bin_mod(cx: &mut ExtCtxt, m: &Mod, hotswap_fns: &mut HotswapFnList) -> Vec<P<Item>> {
    let mut new_items = Vec::new();

    // TODO: look for a way to load the crates that does
    // not require them to be a dependency of the client.
    new_items.push(quote_item!(cx, extern crate libloading;).unwrap());

    for item in &m.items {
        let attr_names = item_attr_names(&item);

        let item = if attr_names.contains("hotswap") {
            expand_bin_fn(cx, item, hotswap_fns)
        } else {
            item.clone()
        };

        new_items.push(item);
    }

    // Create one global variable for each hotswapped function.
    new_items.extend(runtime::create_static_items(cx, hotswap_fns));

    new_items
}

fn expand_bin_fn(cx: &mut ExtCtxt, item: &Item, hotswap_fns: &mut HotswapFnList) -> P<Item> {
    let mut new_item = item.clone();

    if let &mut ItemKind::Fn(ref fn_decl, _, _, _, _, ref mut block) = &mut new_item.node {
        let fn_info = HotswapFnInfo {
            name: item_name(item),
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

    P(new_item)
}
