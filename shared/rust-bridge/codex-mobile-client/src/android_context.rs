use jni::JNIEnv;
use jni::objects::{GlobalRef, JClass, JObject, JString};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jstring};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static ANDROID_CONTEXT_REF: OnceLock<GlobalRef> = OnceLock::new();
static ANDROID_CONTEXT_INITIALIZED: AtomicBool = AtomicBool::new(false);
static ANDROID_CONTEXT_INIT_LOCK: Mutex<()> = Mutex::new(());

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remora_android_core_bridge_UniffiInit_nativeMobileClientInit(
    mut env: JNIEnv,
    _class: JClass,
    context: JObject,
    home_dir: JString,
    codex_home_dir: JString,
) -> jboolean {
    if ANDROID_CONTEXT_INITIALIZED.load(Ordering::Acquire) {
        return JNI_TRUE;
    }

    let _init_guard = match ANDROID_CONTEXT_INIT_LOCK.lock() {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("[remora] Android init lock is poisoned: {error}");
            return JNI_FALSE;
        }
    };
    if ANDROID_CONTEXT_INITIALIZED.load(Ordering::Acquire) {
        return JNI_TRUE;
    }

    let home: String = match env.get_string(&home_dir) {
        Ok(value) => value.into(),
        Err(error) => {
            eprintln!("[remora] Android init could not read HOME: {error}");
            return JNI_FALSE;
        }
    };
    let codex_home: String = match env.get_string(&codex_home_dir) {
        Ok(value) => value.into(),
        Err(error) => {
            eprintln!("[remora] Android init could not read CODEX_HOME: {error}");
            return JNI_FALSE;
        }
    };

    if let Err(error) = bootstrap_android_environment(Path::new(&home), Path::new(&codex_home)) {
        eprintln!("[remora] Android environment init failed: {error}");
        return JNI_FALSE;
    }

    let java_vm = match env.get_java_vm() {
        Ok(java_vm) => java_vm,
        Err(error) => {
            eprintln!("[remora] Android init could not access JavaVM: {error}");
            return JNI_FALSE;
        }
    };
    let context_ref = match env.new_global_ref(context) {
        Ok(context_ref) => context_ref,
        Err(error) => {
            eprintln!("[remora] Android init could not retain application context: {error}");
            return JNI_FALSE;
        }
    };

    let java_vm_ptr = java_vm.get_java_vm_pointer().cast::<c_void>();
    let context_ptr = context_ref.as_obj().as_raw().cast::<c_void>();

    if ANDROID_CONTEXT_REF.set(context_ref).is_err() {
        eprintln!("[remora] Android application context was already retained");
        return JNI_FALSE;
    }

    unsafe {
        ndk_context::initialize_android_context(java_vm_ptr, context_ptr);
    }
    ANDROID_CONTEXT_INITIALIZED.store(true, Ordering::Release);
    JNI_TRUE
}

fn bootstrap_android_environment(home: &Path, codex_home: &Path) -> Result<(), String> {
    std::fs::create_dir_all(codex_home)
        .map_err(|error| format!("creating CODEX_HOME {}: {error}", codex_home.display()))?;

    unsafe {
        std::env::set_var("HOME", home);
        std::env::set_var("CODEX_HOME", codex_home);
    }

    if std::env::var_os("TMPDIR").is_none() {
        let tmpdir = home.join("tmp");
        std::fs::create_dir_all(&tmpdir)
            .map_err(|error| format!("creating TMPDIR {}: {error}", tmpdir.display()))?;
        unsafe {
            std::env::set_var("TMPDIR", &tmpdir);
        }
    }

    install_tls_roots(codex_home)
}

fn install_tls_roots(codex_home: &Path) -> Result<(), String> {
    if std::env::var_os("SSL_CERT_FILE")
        .map(PathBuf::from)
        .is_some_and(|path| path.is_file())
    {
        return Ok(());
    }

    let pem_path = codex_home.join("cacert.pem");
    if !pem_path.is_file() {
        static CACERT_PEM: &[u8] = include_bytes!("cacert.pem");
        std::fs::write(&pem_path, CACERT_PEM)
            .map_err(|error| format!("writing TLS roots {}: {error}", pem_path.display()))?;
    }
    unsafe {
        std::env::set_var("SSL_CERT_FILE", &pem_path);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remora_android_core_bridge_UniffiInit_nativeMobileClientContextProbe(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let message = match std::panic::catch_unwind(|| {
        let context = ndk_context::android_context();
        let _ = context.vm();
        let _ = context.context();

        let _resolver = iroh::dns::DnsResolver::new();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("building tokio runtime: {error}"))?;
        runtime
            .block_on(async {
                let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                    .bind()
                    .await
                    .map_err(|error| format!("binding iroh endpoint: {error}"))?;
                endpoint.close().await;
                Ok::<(), String>(())
            })
            .map_err(|error| format!("probing iroh endpoint: {error}"))?;

        Ok::<String, String>("ok".to_string())
    }) {
        Ok(Ok(message)) => message,
        Ok(Err(message)) => format!("error: {message}"),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|value| (*value).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            format!("panic: {message}")
        }
    };

    env.new_string(message)
        .unwrap_or_else(|_| JString::from(JObject::null()))
        .into_raw()
}
