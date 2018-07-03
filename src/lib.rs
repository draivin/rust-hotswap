#![feature(quote, plugin_registrar, rustc_private, box_syntax, stmt_expr_attributes)]

extern crate rustc_plugin;
extern crate rustc_target;
extern crate syntax;

use rustc_plugin::registry::Registry;

use rustc_target::spec::abi::Abi;
use syntax::ast::{Attribute, Ident, Item, ItemKind, MetaItem, Mod, Name, Ty, VisibilityKind};
use syntax::attr;
use syntax::codemap::Span;
use syntax::edition::Edition;
use syntax::ext::base::SyntaxExtension::{MultiModifier, NormalTT};
use syntax::ext::base::{
    Annotatable, ExtCtxt, MacEager, MacResult, MultiItemModifier, TTMacroExpander,
};
use syntax::feature_gate::AttributeType;
use syntax::ptr::P;
use syntax::tokenstream::TokenStream;

use std::cell::RefCell;
use std::rc::Rc;

use std::mem;

mod codegen;
mod util;

use util::{mod_walk, rustc::*, syntax::get_fn_info};

#[plugin_registrar]
pub fn plugin_registrar(reg: &mut Registry) {
    let fn_list = Rc::new(RefCell::new(Vec::new()));

    let header_extension = HotswapHeaderExtension {
        fn_list: Rc::clone(&fn_list),
    };
    let macro_extension = HotswapMacroExtension {
        fn_list: Rc::clone(&fn_list),
    };

    // This macro is used to walk around the program modules and modify
    // the function code depending on the build type(bin or lib).
    reg.register_syntax_extension(
        Name::intern("hotswap_header"),
        MultiModifier(box header_extension),
    );

    // The user should have a `hotswap_start!` macro before using any
    // hotswapped functions, so the library can initialize all the
    // necessary stuff.
    reg.register_syntax_extension(
        Name::intern("hotswap_start"),
        NormalTT {
            expander: box macro_extension,
            def_info: None,
            allow_internal_unsafe: false,
            allow_internal_unstable: false,
            unstable_feature: None,
            edition: Edition::Edition2015,
        },
    );

    // This macro is used only as a tag so the hotswap header can find out
    // which functions should be hotswapped.
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

// When building a lib, we should export all functions that are tagged as `hotswap`,
// when building a bin, we should completely replace function bodies so it calls
// a dynamically loaded one that is stored in a global structure.
impl MultiItemModifier for HotswapHeaderExtension {
    fn expand(
        &self,
        cx: &mut ExtCtxt,
        _: Span,
        _: &MetaItem,
        annotatable: Annotatable,
    ) -> Vec<Annotatable> {
        if let Annotatable::Item(item) = annotatable {
            let mut item = item.into_inner();

            if let ItemKind::Mod(m) = item.node {
                item.node = ItemKind::Mod(match crate_type().as_ref() {
                    "bin" => {
                        let mut hotswap_fns = self.fn_list.borrow_mut();
                        let tmp = expand_bin_mod(cx, m, &mut hotswap_fns);
                        expand_bin_footer(cx, tmp, &mut hotswap_fns)
                    }
                    "dylib" => expand_lib_mod(cx, m),
                    _ => unimplemented!(),
                });

                // Ignore dead code in the lib build, probably there will be a lot
                // of it, including the `main` function.
                item.attrs = match crate_type().as_ref() {
                    "dylib" => expand_lib_attrs(cx, item.attrs),
                    _ => item.attrs,
                };

                return vec![Annotatable::Item(P(item))];
            }
        }

        // TODO: proper warning when the header annotation is
        // used outside a module.
        unimplemented!();
    }
}

impl TTMacroExpander for HotswapMacroExtension {
    fn expand(&self, cx: &mut ExtCtxt, _: Span, tt: TokenStream) -> Box<MacResult> {
        let hotswap_fns = self.fn_list.borrow();

        // It will be empty when there are no functions tagged as `hotswap`,
        // when `hotswap_header` was no called, and on lib builds, in all
        // those cases we shouldn't expand the hotswap initialization.
        if hotswap_fns.is_empty() {
            // Some arbitrary unsafe code to prevent unnunsed unsafe
            // warnings, also will stop the build during the lib stage,
            // if the user hasn't wrapped the macro in unsafe, instead of
            // building the lib and stopping on the bin.
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

fn expand_lib_attrs(cx: &mut ExtCtxt, mut attrs: Vec<Attribute>) -> Vec<Attribute> {
    attrs.insert(0, quote_attr!(cx, #![allow(unused_imports)]));
    attrs.insert(0, quote_attr!(cx, #![allow(dead_code)]));
    attrs.insert(0, quote_attr!(cx, #![allow(unused_features)]));
    attrs
}

fn expand_lib_mod(cx: &mut ExtCtxt, m: Mod) -> Mod {
    mod_walk(m, &mut |item| {
        if attr::contains_name(&item.attrs, "hotswap") {
            match item.node {
                ItemKind::Fn(..) => return expand_lib_fn(cx, item),
                // TODO: write proper warning.
                _ => println!("warning: hotswap only works on functions"),
            }
            expand_lib_fn(cx, item)
        } else {
            item
        }
    })
}

fn expand_lib_fn(cx: &mut ExtCtxt, mut item: Item) -> Item {
    if let ItemKind::Fn(_, ref mut header, _, _) = item.node {
        // Make lib functions extern and no mangle so they can
        // be imported from the runtime.
        item.attrs.push(quote_attr!(cx, #![no_mangle]));
        item.vis.node = VisibilityKind::Public;

        mem::replace(&mut header.abi, Abi::Rust);
    }

    item
}

fn expand_bin_mod(cx: &mut ExtCtxt, m: Mod, hotswap_fns: &mut HotswapFnList) -> Mod {
    mod_walk(m, &mut |item: Item| {
        if attr::contains_name(&item.attrs, "hotswap") {
            match item.node {
                ItemKind::Fn(..) => return expand_bin_fn(cx, item, hotswap_fns),
                // TODO: write proper warning.
                _ => println!("warning: hotswap only works on functions"),
            }
        }

        item
    })
}

fn expand_bin_fn(cx: &mut ExtCtxt, mut item: Item, hotswap_fns: &mut HotswapFnList) -> Item {
    let fn_info = get_fn_info(cx, &item);

    if let ItemKind::Fn(_, _, _, ref mut block) = item.node {
        mem::replace(block, codegen::fn_body(cx, &fn_info));
    }

    hotswap_fns.push(fn_info);
    item
}

// After all the functions to be hotswapped are found, we insert a custom module
// at the end of the users main file, in which we store the external function
// pointers during runtime.
fn expand_bin_footer(cx: &mut ExtCtxt, mut m: Mod, hotswap_fns: &mut HotswapFnList) -> Mod {
    m.items
        .insert(0, quote_item!(cx, extern crate hotswap_runtime;).unwrap());
    m.items.push(codegen::runtime_mod(cx, hotswap_fns));
    m
}
