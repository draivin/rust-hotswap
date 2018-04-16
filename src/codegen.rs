use syntax::ast::{Block, Expr, Ident, Item, Name};
use syntax::ext::base::ExtCtxt;
use syntax::ptr::P;

use util::rustc::crate_name;
use util::syntax::comma_separated_tokens;

use HotswapFnInfo;

// Creates a module with the runtime structs and a static pointer for each hotswapped function.
pub fn runtime_mod(cx: &mut ExtCtxt, hotswap_fns: &[HotswapFnInfo]) -> P<Item> {
    let mut static_items = Vec::new();

    for hotswap_fn in hotswap_fns {
        let pointer_ident = pointer_ident(&hotswap_fn.name);
        let input_types = &hotswap_fn.input_types;
        let output_types = &hotswap_fn.output_type;

        let item = quote_item!(cx,
            #[allow(non_upper_case_globals)]
            pub static $pointer_ident: RwLock<Option<Arc<fn($input_types) -> $output_types>>> =
                RwLock::new(None);
        ).unwrap();

        static_items.push(item);
    }

    quote_item!(cx,
        #[allow(non_snake_case)]
        #[allow(dead_code)]
        mod _HOTSWAP_RUNTIME {
            use ::std::sync::Arc;
            use ::hotswap_runtime::parking_lot::RwLock;

            $static_items
        }
    ).unwrap()
}

pub fn fn_body(cx: &mut ExtCtxt, fn_info: &HotswapFnInfo) -> P<Block> {
    let pointer_name = &fn_info.name;
    let pointer_ident = pointer_ident(pointer_name);
    let input_idents = comma_separated_tokens(cx, &fn_info.input_idents);

    P(quote_block!(cx, {
        let func = {
            let guard = ::_HOTSWAP_RUNTIME::$pointer_ident.read();
            match *guard {
                Some(ref arc) => arc.clone(),
                None => panic!(
                    "Hotswapped function `{}` called before `hotswap_start!()` invocation!",
                    $pointer_name
                )
            }
        };

        func($input_idents)
    }).into_inner())
}

pub fn macro_expansion(cx: &mut ExtCtxt, hotswap_fns: &[HotswapFnInfo]) -> P<Expr> {
    let mut ref_updaters = Vec::new();

    // Create one statement per hotswapped function, each
    // statement will update its global variable to point
    // to the latest dynamic address, and save the previous
    // reference so it can be used for refcounting.
    for fn_info in hotswap_fns.iter() {
        let pointer_name = &fn_info.name;
        let pointer_ident = pointer_ident(pointer_name);
        let input_types = comma_separated_tokens(cx, &fn_info.input_types);
        let output_type = &fn_info.output_type;

        let stmt = quote_stmt!(cx, {
            let fn_address =
                *lib.get::<fn($input_types) -> $output_type>($pointer_name.as_bytes())
                .expect(&format!(
                    "Couldn't find function `{}` on hotswapped library",
                    $pointer_name
                )).deref();

            let mut pointer_guard = ::_HOTSWAP_RUNTIME::$pointer_ident.write();
            let new_ref = Some(Arc::new(fn_address));
            let prev_ref = mem::replace(&mut *pointer_guard, new_ref);

            if let Some(ref mut lib) = current_lib {
                if let Some(arc) = prev_ref {
                    lib.add_ref(arc);
                }
            }
        }).unwrap();

        ref_updaters.push(stmt);
    }

    #[cfg(target_os = "windows")]
    let dylib_name_template = crate_name() + "{}.dll";

    #[cfg(target_os = "macos")]
    let dylib_name_template = crate_name() + "{}.dylib";

    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "dragonfly"))]
    let dylib_name_template = "lib".to_string() + &crate_name() + "{}.so";

    let dylib_name = dylib_name_template.replace("{}", "");

    let block = quote_expr!(cx, {
        use ::std::{fs, mem, thread};
        use ::std::env::current_exe;
        use ::std::ops::Deref;
        use ::std::sync::Arc;

        use ::hotswap_runtime::libloading::Library;
        use ::hotswap_runtime::parking_lot::Mutex;
        use ::hotswap_runtime::RefManager;

        let exe = current_exe().expect("Couldn't find current executable name");
        let dir = exe.parent().expect("Couldn't find executable path");

        // TODO: warn if dynamic library was not found.
        let tmp_path = dir.join("hotswap-dylib");
        let dylib_file = dir.join($dylib_name);
        let dylib_move = dylib_file.clone();

        let mut last_modified = fs::metadata(&dylib_file).expect(
            &format!(
                "Couldn't find metadata for {} - did you add a `[lib]` section to your Cargo.toml?",
                dylib_file.to_string_lossy()
            )
        ).modified().unwrap();

        // Keep a list of all the old libs so we can refcount and drop as needed.
        let old_libs: Arc<Mutex<Vec<RefManager>>> = Arc::new(Mutex::new(Vec::new()));
        let old_libs_move = old_libs.clone();

        let mut current_lib: Option<RefManager> = None;

        let mut reload_dylib = move |dylib_num| {
            // Windows locks the dynamic library once it is loaded, so
            // I'm creating a copy for now.
            let copy_name = format!($dylib_name_template, dylib_num);

            let mut dylib_copy = tmp_path.clone();
            fs::create_dir_all(&tmp_path).expect(
                "Couldn't create temp folder for the new dynamic library"
            );

            dylib_copy.push(copy_name);
            fs::copy(&dylib_move, &dylib_copy).expect(
                "Couldn't copy the dynamic library, maybe the destination has too-restrictive \
                 permissions"
            );

            let lib = Library::new(dylib_copy.to_string_lossy().as_ref())
                .expect("Failed to load library");

            // Inline the function reference updaters.
            $ref_updaters

            // This should happen after the ref_updaters run, otherwise
            // references to the previous library functions will be added
            // to this RefManager.
            let new_lib = Some(RefManager::new(lib));
            let old_lib = mem::replace(&mut current_lib, new_lib);

            if let Some(lib) = old_lib {
                old_libs.lock().push(lib);
            }
        };

        reload_dylib(0);

        thread::spawn(move || {
            let mut dylib_num = 1;

            loop {
                thread::sleep(std::time::Duration::from_millis(5000));

                // Check if any of the currently loaded libraries can
                // be dropped, if so, drop them.
                {
                    let mut old_libs_move = old_libs_move.lock();
                    for i in (0..old_libs_move.len()).rev() {
                        if old_libs_move[i].should_drop() {
                            old_libs_move.remove(i);
                        }
                    }
                }

                // TODO: use some filesystem notification crate
                // so it reloads as soon as the file changes.
                let modified = match fs::metadata(&dylib_file) {
                    Ok(metadata) => metadata.modified().expect(
                        "Couldn't get the metadata's modified time on this platform, hot reloading \
                         will be horribly inefficient. If you want to use hot-reloading anyway, \
                         file a PR to do something sensible on this platform."
                    ),
                    _ => continue,
                };

                if modified > last_modified {
                    last_modified = modified;
                    reload_dylib(dylib_num);
                    dylib_num += 1;
                }
            }
        });
    }).into_inner();

    P(block)
}

fn pointer_ident(fn_name: &str) -> Ident {
    Ident::with_empty_ctxt(Name::intern(&("_HOTSWAP_".to_string() + fn_name)))
}
