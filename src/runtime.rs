use syntax::ast::{Block, Expr, Ident, Item};
use syntax::ext::base::ExtCtxt;
use syntax::parse::token::intern;
use syntax::ptr::P;

use ::{HotswapFnList, HotswapFnInfo};
use ::util::rustc::{crate_name};
use ::util::syntax::comma_separated_tokens;

// Creates a module with a static pointer for each hotswapped function.
pub fn create_hotswap_mod(cx: &mut ExtCtxt, hotswap_fns: &HotswapFnList) -> P<Item> {
    let mut static_items = Vec::new();

    let atomic_usize = quote_ty!(cx, ::std::sync::atomic::AtomicUsize);
    let atomic_usize_init = P(quote_expr!(cx, ::std::sync::atomic::ATOMIC_USIZE_INIT).unwrap());

    for hotswap_fn in hotswap_fns {
        let dyn_pointer = pointer_ident(&hotswap_fn.name);
        let item = quote_item!(cx,
            #[allow(non_upper_case_globals)]
            pub static $dyn_pointer: $atomic_usize = $atomic_usize_init;
        ).unwrap();

        static_items.push(item);
    }

    quote_item!(cx,
        #[allow(non_snake_case)]
        mod _HOTSWAP_RUNTIME {
            $static_items
        }
    ).unwrap()
}

pub fn create_fn_body(cx: &mut ExtCtxt, fn_info: &HotswapFnInfo) -> P<Block> {
    let arg_idents = comma_separated_tokens(cx, &fn_info.input_idents);
    let arg_types = comma_separated_tokens(cx, &fn_info.input_types);
    let dyn_pointer = pointer_ident(&fn_info.name);
    let ret = &fn_info.output_type;

    P(quote_block!(cx, {
        let func = unsafe {
            use std::mem::transmute;
            use std::sync::atomic::Ordering;
            transmute::<_, extern "Rust" fn($arg_types) -> $ret>(
                ::_HOTSWAP_RUNTIME::$dyn_pointer.load(Ordering::Relaxed))
        };

        func($arg_idents)
    }).unwrap())
}

pub fn create_macro_expansion(cx: &mut ExtCtxt, hotswap_fns: &HotswapFnList) -> P<Expr> {
    let mut ref_updaters = Vec::new();

    // Create one statement per hotswapped function, each
    // statement will update its global variable to point
    // to the latest dynamic address.
    for hotswap_fn in hotswap_fns.iter() {
        let name = &hotswap_fn.name;
        let global_ident = pointer_ident(name);

        let stmt = quote_stmt!(cx, {
            use std::sync::atomic::Ordering;

            let fn_address = *lib.get::<fn()>($name.as_bytes()).unwrap().deref();
            ::_HOTSWAP_RUNTIME::$global_ident.store(fn_address as usize, Ordering::Relaxed);
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
    }).unwrap();

    P(block)
}

fn pointer_ident(fn_name: &str) -> Ident {
    Ident::with_empty_ctxt(intern(&("_HOTSWAP_".to_string() + fn_name)))
}
