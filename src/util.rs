pub mod syntax {
    use syntax::ast::{FnDecl, FunctionRetTy, Ident, Item, MetaItemKind, PatKind, TokenTree, Ty};
    use syntax::codemap;
    use syntax::ext::base::ExtCtxt;
    use syntax::ext::quote::rt::ToTokens;
    use syntax::parse::token;
    use syntax::ptr::P;

    use std::collections::HashSet;

    pub fn comma_separated_tokens<T: ToTokens>(cx: &mut ExtCtxt, entries: &[T]) -> Vec<TokenTree> {
        entries.iter()
            .map(|t| t.to_tokens(cx))
            .collect::<Vec<_>>()
            .join(&TokenTree::Token(codemap::DUMMY_SP, token::Comma))
    }

    pub fn arg_idents(decl: &FnDecl) -> Vec<Ident> {
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

    pub fn arg_types(fn_decl: &FnDecl) -> Vec<Ty> {
        fn_decl.inputs.iter().map(|arg| (*arg.ty).clone()).collect()
    }

    pub fn return_type(cx: &mut ExtCtxt, fn_decl: &FnDecl) -> P<Ty> {
        match &fn_decl.output {
            &FunctionRetTy::Ty(ref ty) => ty.clone(),
            _ => quote_ty!(cx, ()),
        }
    }

    pub fn item_name(item: &Item) -> String {
        format!("{}", item.ident.name)
    }

    pub fn item_attr_names(item: &Item) -> HashSet<&str> {
        let mut attr_names = HashSet::new();

        for attr in &item.attrs {
            if let &MetaItemKind::Word(ref word) = &attr.node.value.node {
                attr_names.insert(&**word);
            }
        }

        attr_names
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
