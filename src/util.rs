use syntax::{ast::{Item, ItemKind, Mod, VisibilityKind},
             ptr::P};

pub fn mod_walk(mut m: Mod, item_map: &mut FnMut(Item) -> Item) -> Mod {
    m.items = m.items
        .into_iter()
        .map(|item| {
            let mut item = item.into_inner();
            let mut should_map = false;

            item.node = match item.node {
                ItemKind::Mod(m) => {
                    item.vis.node = VisibilityKind::Public;
                    ItemKind::Mod(mod_walk(m, item_map))
                }
                _ => {
                    should_map = true;
                    item.node
                }
            };

            if should_map {
                P(item_map(item))
            } else {
                P(item)
            }
        })
        .collect();

    m
}
pub mod syntax {
    use syntax::ast::{FnDecl, FunctionRetTy, Ident, Item, ItemKind, PatKind, Ty};
    use syntax::codemap;
    use syntax::ext::base::ExtCtxt;
    use syntax::ext::quote::rt::ToTokens;
    use syntax::parse::token;
    use syntax::ptr::P;
    use syntax::tokenstream::TokenTree;

    use HotswapFnInfo;

    pub fn get_fn_info(cx: &mut ExtCtxt, item: &Item) -> HotswapFnInfo {
        if let ItemKind::Fn(ref fn_decl, _, _, _) = item.node {
            HotswapFnInfo {
                name: ident_name(&item.ident),
                input_types: arg_types(fn_decl),
                input_idents: arg_idents(fn_decl),
                output_type: return_type(cx, fn_decl),
            }
        } else {
            unreachable!();
        }
    }

    pub fn comma_separated_tokens<T: ToTokens>(cx: &mut ExtCtxt, entries: &[T]) -> Vec<TokenTree> {
        entries
            .iter()
            .map(|t| t.to_tokens(cx))
            .collect::<Vec<_>>()
            .join(&TokenTree::Token(codemap::DUMMY_SP, token::Comma))
    }

    fn ident_name(ident: &Ident) -> String {
        format!("{}", ident.name)
    }

    fn arg_idents(decl: &FnDecl) -> Vec<Ident> {
        decl.inputs
            .iter()
            .filter_map(|arg| {
                let mut ident = None;
                arg.pat.walk(&mut |pat| {
                    if let PatKind::Ident(_, span_ident, _) = pat.node {
                        ident = Some(span_ident);
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
        match fn_decl.output {
            FunctionRetTy::Ty(ref ty) => ty.clone(),
            _ => quote_ty!(cx, ()),
        }
    }
}

pub mod rustc {
    pub fn arg(arg_name: &str) -> String {
        let args = ::std::env::args();
        let mut args = args.skip_while(|arg| arg != arg_name);
        args.nth(1).expect("Could not find arg")
    }

    pub fn crate_type() -> String {
        arg("--crate-type")
    }

    pub fn crate_name() -> String {
        arg("--crate-name")
    }
}
