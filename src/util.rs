pub mod syntax {
    use syntax::ast::{FnDecl, FunctionRetTy, Ident, Item, ItemKind, PatKind, TokenTree, Ty};
    use syntax::codemap;
    use syntax::ext::base::ExtCtxt;
    use syntax::ext::quote::rt::ToTokens;
    use syntax::parse::token;
    use syntax::ptr::P;

    use ::HotswapFnInfo;

    pub fn get_fn_info(cx: &mut ExtCtxt, item: &Item) -> HotswapFnInfo {
        if let ItemKind::Fn(ref fn_decl, _, _, _, _, _) = item.node {
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
        entries.iter()
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
                    if let PatKind::Ident(_, ref span_ident, _) = pat.node {
                        ident = Some(span_ident.node);
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
        let mut args = ::std::env::args();
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

    pub fn crate_type() -> String {
        arg("--crate-type")
    }

    pub fn crate_name() -> String {
        arg("--crate-name")
    }
}
