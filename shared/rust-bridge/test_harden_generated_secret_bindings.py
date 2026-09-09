#!/usr/bin/env python3
"""Regression tests for generated relay-secret binding hardening."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


SCRIPT_PATH = Path(__file__).with_name("harden-generated-secret-bindings.py")
SPEC = importlib.util.spec_from_file_location("relay_secret_hardener", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
HARDENER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARDENER)


ASYNC_CALL = """            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback
            )
"""


SWIFT_ASYNC_HELPER = """fileprivate func uniffiRustCallAsync<F, T>(
    rustFutureFunc: () -> UInt64,
    pollFunc: (UInt64, @escaping UniffiRustFutureContinuationCallback, UInt64) -> (),
    completeFunc: (UInt64, UnsafeMutablePointer<RustCallStatus>) -> F,
    freeFunc: (UInt64) -> (),
    liftFunc: (F) throws -> T,
    errorHandler: ((RustBuffer) throws -> Swift.Error)?
) async throws -> T {
    // Make sure to call the ensure init function since future creation doesn't have a
    // RustCallStatus param, so doesn't use makeRustCall()
    uniffiEnsureCodexMobileClientInitialized()
    let rustFuture = rustFutureFunc()
    defer {
        freeFunc(rustFuture)
    }
    var pollResult: Int8;
    repeat {
        pollResult = await withUnsafeContinuation {
            pollFunc(
                rustFuture,
                { handle, pollResult in
                    uniffiFutureContinuationCallback(handle: handle, pollResult: pollResult)
                },
                uniffiContinuationHandleMap.insert(obj: $0)
            )
        }
    } while pollResult != UNIFFI_RUST_FUTURE_POLL_READY

    return try liftFunc(makeRustCall(
        { completeFunc(rustFuture, $0) },
        errorHandler: errorHandler
    ))
}
"""


def swift_rust_buffer_method(
    method: str,
    signature: str,
    rust_function: str,
    lowered_arguments: str,
    outcome: str,
) -> str:
    arguments = f"                    {lowered_arguments}\n" if lowered_arguments else ""
    return f"""open func {method}({signature})async throws  -> {outcome}  {{
    return
        try  await uniffiRustCallAsync(
            rustFutureFunc: {{
                {rust_function}(
                    self.uniffiCloneHandle(),
{arguments}                )
            }},
            pollFunc: ffi_codex_mobile_client_rust_future_poll_rust_buffer,
            completeFunc: ffi_codex_mobile_client_rust_future_complete_rust_buffer,
            freeFunc: ffi_codex_mobile_client_rust_future_free_rust_buffer,
            liftFunc: FfiConverterType{outcome}_lift,
            errorHandler: FfiConverterTypeRemoraLinkError_lift
        )
}}
"""


SWIFT_INSPECT_METHOD = swift_rust_buffer_method(
    "inspectRemoraLinkCode",
    "code: AppRemoraLinkPairingCode",
    "uniffi_codex_mobile_client_fn_method_appclient_inspect_remora_link_code",
    "FfiConverterTypeAppRemoraLinkPairingCode_lower(code)",
    "AppRemoraLinkInspection",
)
SWIFT_ACCEPT_METHOD = swift_rust_buffer_method(
    "acceptRemoraLinkOffer",
    "acceptance: AppRemoraLinkAcceptance",
    "uniffi_codex_mobile_client_fn_method_appclient_accept_remora_link_offer",
    "FfiConverterTypeAppRemoraLinkAcceptance_lower(acceptance)",
    "AppRemoraLinkPairingOutcome",
)
SWIFT_AWAIT_METHOD = swift_rust_buffer_method(
    "awaitRemoraLinkPairing",
    "hostId: String, code: AppRemoraLinkPairingCode?",
    "uniffi_codex_mobile_client_fn_method_appclient_await_remora_link_pairing",
    "FfiConverterString.lower(hostId),FfiConverterOptionTypeAppRemoraLinkPairingCode.lower(code)",
    "AppRemoraLinkPairingOutcome",
)


KOTLIN_LEGACY_REGISTRATION = """    job.invokeOnCompletion { onCompletion?.invoke() }
    val handle = uniffiForeignFutureHandleMap.insert(job)
    uniffiOutDroppedCallback.uniffiSetValue(UniffiForeignFutureDroppedCallbackStruct(handle, uniffiForeignFutureDroppedCallbackImpl))
    job.start()
"""


KOTLIN_PROTECTED_REGISTRATION = """    job.invokeOnCompletion { onCompletion?.invoke() }
    val handle = try {
        uniffiForeignFutureHandleMap.insert(job)
    } catch (error: Throwable) {
        try {
            job.cancel()
        } catch (_: Throwable) {
            // Preserve the original registration failure.
        }
        throw error
    }
    try {
        uniffiOutDroppedCallback.uniffiSetValue(
            UniffiForeignFutureDroppedCallbackStruct(
                handle,
                uniffiForeignFutureDroppedCallbackImpl,
            )
        )
        job.start()
    } catch (error: Throwable) {
        try {
            job.cancel()
        } catch (_: Throwable) {
            // Preserve the original publication/start failure.
        }
        try {
            uniffiForeignFutureHandleMap.remove(handle)
        } catch (_: Throwable) {
            // Preserve the original publication/start failure.
        }
        throw error
    }
"""


def async_call_with_error(exception: str, converter: str) -> str:
    return f"""            uniffiTraitInterfaceCallAsyncWithError(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                {{ e: {exception} -> {converter}.lower(e) }},
                uniffiOutDroppedCallback
            )
"""


def swift_value_callback(method: str, outcome: str) -> str:
    return f"""            let makeCall = {{
                () async throws -> {outcome} in
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRelaySecretBackend.handleMap.get(handle: uniffiHandle) else {{
                    throw UniffiInternalError.unexpectedStaleHandle
                }}
                return await uniffiObj.{method}(
                     alias: try FfiConverterString.lift(alias),
                     value: try FfiConverterTypeAppRelaySecretValue_lift(value)
                )
            }}
"""


def swift_secret_success_callback() -> str:
    return """            let uniffiHandleSuccess = { (returnValue: AppRelaySecretValue) in
                uniffiFutureCallback(
                    uniffiCallbackData,
                    UniffiForeignFutureResultRustBuffer(
                        returnValue: FfiConverterTypeAppRelaySecretValue_lower(returnValue),
                        callStatus: RustCallStatus()
                    )
                )
            }
"""


def hardened_swift_secret_success_callback() -> str:
    return swift_secret_success_callback().replace(
        "                uniffiFutureCallback(\n",
        "                defer { returnValue.zeroize() }\n"
        "                uniffiFutureCallback(\n",
        1,
    )


def swift_callback(name: str, body: str) -> str:
    return f"""        {name}: {{ (
            uniffiHandle: UInt64
        ) in
{body}        }},
"""


def swift_backend(name: str, callbacks: str) -> str:
    return f"""fileprivate struct UniffiCallbackInterface{name} {{
    static let vtable: [UniffiVTableCallbackInterface{name}] = [UniffiVTableCallbackInterface{name}(
{callbacks}    )]
}}

private func uniffiCallbackInit{name}() {{}}

"""


def raw_swift_fixture() -> str:
    compare_and_swap = """            let makeCall = {
                () async throws -> AppRelaySecretCasOutcome in
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRelaySecretBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                return await uniffiObj.compareAndSwap(
                     alias: try FfiConverterString.lift(alias),
                     expectedRevision: try FfiConverterOptionUInt64.lift(expectedRevision),
                     replacementRevision: try FfiConverterUInt64.lift(replacementRevision),
                     value: try FfiConverterTypeAppRelaySecretValue_lift(value)
                )
            }
"""
    sign_message = """            let makeCall = {
                () async throws -> AppRelaySecretValue in
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRemoraLinkDeviceKeyBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                return try await uniffiObj.signMessage(
                     slot: try FfiConverterString.lift(slot),
                     message: try FfiConverterTypeAppRelaySecretValue_lift(message)
                )
            }
""" + swift_secret_success_callback()
    load_or_create = """            let makeCall = {
                () async throws -> AppRelaySecretValue in
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRemoraLinkTransportIdentityBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                return try await uniffiObj.loadOrCreate(
                     candidate: try FfiConverterTypeAppRelaySecretValue_lift(candidate)
                )
            }
""" + swift_secret_success_callback()

    return """import Foundation

extension RustBuffer {
    func deallocate() {
        try! rustCall { ffi_codex_mobile_client_rustbuffer_free(self, $0) }
    }
}

public typealias AppRelaySecretValue = Data

#if swift(>=5.8)
@_documentation(visibility: private)
#endif
public struct FfiConverterTypeAppRelaySecretValue: FfiConverter {
    public static func read(from buf: inout (data: Data, offset: Data.Index)) throws -> AppRelaySecretValue {
        return try FfiConverterData.read(from: &buf)
    }

    public static func write(_ value: AppRelaySecretValue, into buf: inout [UInt8]) {
        return FfiConverterData.write(value, into: &buf)
    }

    public static func lift(_ value: RustBuffer) throws -> AppRelaySecretValue {
        return try FfiConverterData.lift(value)
    }

    public static func lower(_ value: AppRelaySecretValue) -> RustBuffer {
        return FfiConverterData.lower(value)
    }
}

public typealias AppRemoraLinkPairingCode = Data

#if swift(>=5.8)
@_documentation(visibility: private)
#endif
public struct FfiConverterTypeAppRemoraLinkPairingCode: FfiConverter {
    public static func read(from buf: inout (data: Data, offset: Data.Index)) throws -> AppRemoraLinkPairingCode {
        return try FfiConverterData.read(from: &buf)
    }

    public static func write(_ value: AppRemoraLinkPairingCode, into buf: inout [UInt8]) {
        return FfiConverterData.write(value, into: &buf)
    }

    public static func lift(_ value: RustBuffer) throws -> AppRemoraLinkPairingCode {
        return try FfiConverterData.lift(value)
    }

    public static func lower(_ value: AppRemoraLinkPairingCode) -> RustBuffer {
        return FfiConverterData.lower(value)
    }
}

fileprivate struct FfiConverterOptionTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer {
    typealias SwiftType = AppRemoraLinkPairingCode?

    public static func write(_ value: SwiftType, into buf: inout [UInt8]) {
        guard let value = value else {
            writeInt(&buf, Int8(0))
            return
        }
        writeInt(&buf, Int8(1))
        FfiConverterTypeAppRemoraLinkPairingCode.write(value, into: &buf)
    }

    public static func read(from buf: inout (data: Data, offset: Data.Index)) throws -> SwiftType {
        switch try readInt(&buf) as Int8 {
        case 0: return nil
        case 1: return try FfiConverterTypeAppRemoraLinkPairingCode.read(from: &buf)
        default: throw UniffiInternalError.unexpectedOptionalTag
        }
    }
}

""" + swift_backend(
        "AppRelaySecretBackend",
        swift_callback("read", swift_secret_success_callback())
        + swift_callback(
            "write", swift_value_callback("write", "AppRelaySecretWriteOutcome")
        )
        + swift_callback(
            "createIfAbsent",
            swift_value_callback("createIfAbsent", "AppRelaySecretCreateOutcome"),
        )
        + swift_callback("compareAndSwap", compare_and_swap),
    ) + swift_backend(
        "AppRemoraLinkDeviceKeyBackend",
        swift_callback("signMessage", sign_message),
    ) + swift_backend(
        "AppRemoraLinkTransportIdentityBackend",
        swift_callback("loadOrCreate", load_or_create),
    ) + """

func pushOne(token: AppRelaySecretValue) {}
func pushTwo(token: AppRelaySecretValue) {}
open func ordinaryOperation()async throws   {
    return
        try  await uniffiRustCallAsync(
            rustFutureFunc: {
                uniffi_codex_mobile_client_fn_method_appclient_ordinary_operation(
                    self.uniffiCloneHandle()
                )
            },
            pollFunc: ffi_codex_mobile_client_rust_future_poll_void,
            completeFunc: ffi_codex_mobile_client_rust_future_complete_void,
            freeFunc: ffi_codex_mobile_client_rust_future_free_void,
            liftFunc: { $0 },
            errorHandler: FfiConverterTypeBackgroundRelayError_lift
        )
}
func signMessage(message: AppRelaySecretValue) {}
func loadOrCreate(candidate: AppRelaySecretValue) {}
func inspectRemoraLinkCode(code: AppRemoraLinkPairingCode) async throws
func acceptRemoraLinkOffer(acceptance: AppRemoraLinkAcceptance) async throws
func awaitRemoraLinkPairing(hostId: String, code: AppRemoraLinkPairingCode?) async throws

""" + SWIFT_INSPECT_METHOD + SWIFT_ACCEPT_METHOD + SWIFT_AWAIT_METHOD + """

private let UNIFFI_RUST_FUTURE_POLL_READY: Int8 = 0
private let UNIFFI_RUST_FUTURE_POLL_WAKE: Int8 = 1

fileprivate let uniffiContinuationHandleMap = UniffiHandleMap<UnsafeContinuation<Int8, Never>>()

""" + SWIFT_ASYNC_HELPER + """

// Callback handlers for an async calls.  These are invoked by Rust when the future is ready.  They
// lift the return value or error and resume the suspended function.
fileprivate func uniffiFutureContinuationCallback(handle: UInt64, pollResult: Int8) {
    if let continuation = try? uniffiContinuationHandleMap.remove(handle: handle) {
        continuation.resume(returning: pollResult)
    } else {
        print("uniffiFutureContinuationCallback invalid handle")
    }
}
"""


def callback(name: str, body: str) -> str:
    return f"""    internal object `{name}`: CallbackMethod {{
{body}    }}
"""


def raw_kotlin_fixture() -> str:
    read = """            val uniffiHandleSuccess = { returnValue: AppRelaySecretValue ->
                val uniffiResult = UniffiForeignFutureResultRustBuffer.UniffiByValue(
                    FfiConverterTypeAppRelaySecretValue.lower(returnValue),
                    UniffiRustCallStatus.ByValue()
                )
                uniffiResult.write()
                uniffiFutureCallback.callback(uniffiCallbackData, uniffiResult)
            }
"""
    write = """            val uniffiObj = FfiConverterTypeAppRelaySecretBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`write`(
                    FfiConverterString.lift(`alias`),
                    FfiConverterTypeAppRelaySecretValue.lift(`value`),
                )
            }
""" + ASYNC_CALL
    create = """            val uniffiObj = FfiConverterTypeAppRelaySecretBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`createIfAbsent`(
                    FfiConverterString.lift(`alias`),
                    FfiConverterTypeAppRelaySecretValue.lift(`value`),
                )
            }
""" + ASYNC_CALL
    compare_and_swap = """            val uniffiObj = FfiConverterTypeAppRelaySecretBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`compareAndSwap`(
                    FfiConverterString.lift(`alias`),
                    FfiConverterOptionalULong.lift(`expectedRevision`),
                    FfiConverterULong.lift(`replacementRevision`),
                    FfiConverterTypeAppRelaySecretValue.lift(`value`),
                )
            }
""" + ASYNC_CALL
    sign_message = """            val uniffiObj = FfiConverterTypeAppRemoraLinkDeviceKeyBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`signMessage`(
                    FfiConverterString.lift(`slot`),
                    FfiConverterTypeAppRelaySecretValue.lift(`message`),
                )
            }
""" + read + async_call_with_error(
        "AppRemoraLinkDeviceKeyException",
        "FfiConverterTypeAppRemoraLinkDeviceKeyError",
    )
    load_or_create = """            val uniffiObj = FfiConverterTypeAppRemoraLinkTransportIdentityBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`loadOrCreate`(
                    FfiConverterTypeAppRelaySecretValue.lift(`candidate`),
                )
            }
""" + read + async_call_with_error(
        "AppRemoraLinkTransportIdentityException",
        "FfiConverterTypeAppRemoraLinkTransportIdentityError",
    )

    return """public typealias AppRelaySecretValue = kotlin.ByteArray
public typealias FfiConverterTypeAppRelaySecretValue = FfiConverterByteArray

public typealias AppRemoraLinkPairingCode = kotlin.ByteArray
public typealias FfiConverterTypeAppRemoraLinkPairingCode = FfiConverterByteArray

public object FfiConverterOptionalTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer<AppRemoraLinkPairingCode?> {
    override fun read(buf: ByteBuffer): AppRemoraLinkPairingCode? {
        if (buf.get().toInt() == 0) {
            return null
        }
        return FfiConverterTypeAppRemoraLinkPairingCode.read(buf)
    }

    override fun allocationSize(value: AppRemoraLinkPairingCode?): ULong {
        if (value == null) {
            return 1UL
        } else {
            return 1UL + FfiConverterTypeAppRemoraLinkPairingCode.allocationSize(value)
        }
    }

    override fun write(value: AppRemoraLinkPairingCode?, buf: ByteBuffer) {
        if (value == null) {
            buf.put(0)
        } else {
            buf.put(1)
            FfiConverterTypeAppRemoraLinkPairingCode.write(value, buf)
        }
    }
}

internal inline fun<T> uniffiTraitInterfaceCallAsync(
    crossinline makeCall: suspend () -> T,
    crossinline handleSuccess: (T) -> Unit,
    crossinline handleError: (UniffiRustCallStatus.ByValue) -> Unit,
    uniffiOutDroppedCallback: UniffiForeignFutureDroppedCallbackStruct,
) {
    val job = GlobalScope.launch coroutineBlock@ {
        // Note: it's important we call either `handleSuccess` or `handleError` exactly once.  Each
        handleSuccess(callResult)
    }
    val handle = uniffiForeignFutureHandleMap.insert(job)
    uniffiOutDroppedCallback.uniffiSetValue(UniffiForeignFutureDroppedCallbackStruct(handle, uniffiForeignFutureDroppedCallbackImpl))
}

internal inline fun<T, reified E: Throwable> uniffiTraitInterfaceCallAsyncWithError(
    crossinline makeCall: suspend () -> T,
    crossinline handleSuccess: (T) -> Unit,
    crossinline handleError: (UniffiRustCallStatus.ByValue) -> Unit,
    crossinline lowerError: (E) -> RustBuffer.ByValue,
    uniffiOutDroppedCallback: UniffiForeignFutureDroppedCallbackStruct,
) {
    @OptIn(DelicateCoroutinesApi::class)
    val job = GlobalScope.launch coroutineBlock@ {
        // See the note in uniffiTraitInterfaceCallAsync for details on `handleSuccess` and
        handleSuccess(callResult)
    }
    val handle = uniffiForeignFutureHandleMap.insert(job)
    uniffiOutDroppedCallback.uniffiSetValue(UniffiForeignFutureDroppedCallbackStruct(handle, uniffiForeignFutureDroppedCallbackImpl))
}

internal val uniffiForeignFutureHandleMap = UniffiHandleMap<Job>()

""" + """internal object uniffiCallbackInterfaceAppRelaySecretBackend {
""" + callback("read", read) + callback("write", write) + callback(
        "createIfAbsent", create
    ) + callback("revision", ASYNC_CALL) + callback(
        "compareAndSwap", compare_and_swap
    ) + callback("compareAndTombstone", ASYNC_CALL) + callback(
        "delete", ASYNC_CALL
    ) + """
    internal object uniffiFree: CallbackMethod
}

internal object uniffiCallbackInterfaceAppRemoraLinkDeviceKeyBackend {
""" + callback("ensureHardwareKey", ASYNC_CALL) + callback(
        "loadHardwareKey", ASYNC_CALL
    ) + callback("signMessage", sign_message) + callback(
        "deleteHardwareKey", ASYNC_CALL
    ) + """
    internal object uniffiFree: CallbackMethod
}

internal object uniffiCallbackInterfaceAppRemoraLinkTransportIdentityBackend {
""" + callback("loadOrCreate", load_or_create) + """
    internal object uniffiFree: CallbackMethod
}

fun pushOne(`token`: AppRelaySecretValue) = Unit
fun pushTwo(`token`: AppRelaySecretValue) = Unit
fun signMessage(`message`: AppRelaySecretValue) = Unit
fun loadOrCreate(`candidate`: AppRelaySecretValue) = Unit
    suspend fun `inspectRemoraLinkCode`(`code`: AppRemoraLinkPairingCode)
    suspend fun `awaitRemoraLinkPairing`(`hostId`: kotlin.String, `code`: AppRemoraLinkPairingCode?)

    override suspend fun `inspectRemoraLinkCode`(`code`: AppRemoraLinkPairingCode) {
        use(FfiConverterTypeAppRemoraLinkPairingCode.lower(`code`))
    }

    override suspend fun `awaitRemoraLinkPairing`(`hostId`: kotlin.String, `code`: AppRemoraLinkPairingCode?) {
        use(FfiConverterString.lower(`hostId`),FfiConverterOptionalTypeAppRemoraLinkPairingCode.lower(`code`))
    }
"""


def method_block(source: str, method: str) -> str:
    marker = f"    internal object `{method}`:"
    start = source.index(marker)
    candidates = [
        index
        for index in (
            source.find("\n    internal object `", start + len(marker)),
            source.find("\n    internal object uniffiFree:", start + len(marker)),
        )
        if index >= 0
    ]
    return source[start : min(candidates)]


def kotlin_async_helper_block(source: str, *, fallible: bool) -> str:
    if fallible:
        marker = (
            "internal inline fun<T, reified E: Throwable> "
            "uniffiTraitInterfaceCallAsyncWithError("
        )
        end_marker = "\ninternal val uniffiForeignFutureHandleMap"
    else:
        marker = "internal inline fun<T> uniffiTraitInterfaceCallAsync("
        end_marker = (
            "\ninternal inline fun<T, reified E: Throwable> "
            "uniffiTraitInterfaceCallAsyncWithError("
        )
    start = source.index(marker)
    end = source.index(end_marker, start)
    return source[start:end]


def swift_app_client_method(source: str, method: str) -> str:
    marker = f"open func {method}("
    start = source.index(marker)
    return source[start : source.index("\n}", start) + len("\n}")]


class SecretBindingHardeningTests(unittest.TestCase):
    def test_idempotent_replace_accepts_hardened_prefix_transform(self) -> None:
        raw = "import Foundation\n"
        hardened = raw + "import Darwin\n"

        first = HARDENER.replace_once(raw, raw, hardened, "test import")
        self.assertEqual(first, hardened)
        self.assertEqual(
            HARDENER.replace_once(first, raw, hardened, "test import"), hardened
        )

    def test_cleanup_is_method_scoped_and_full_hardening_is_idempotent(self) -> None:
        hardened = HARDENER.harden_kotlin_source(raw_kotlin_fixture())

        for method in ("write", "createIfAbsent", "compareAndSwap"):
            block = method_block(hardened, method)
            self.assertEqual(block.count("val secretValue ="), 1, method)
            self.assertEqual(block.count("secretValue.fill(0)"), 2, method)

        for method in ("revision", "compareAndTombstone", "delete"):
            self.assertNotIn("secretValue", method_block(hardened, method), method)

        for method in ("signMessage", "loadOrCreate"):
            block = method_block(hardened, method)
            self.assertEqual(block.count("val secretValue ="), 1, method)
            self.assertEqual(block.count("secretValue.fill(0)"), 2, method)
            self.assertEqual(block.count("returnValue.fill(0)"), 1, method)

        self.assertEqual(HARDENER.harden_kotlin_source(hardened), hardened)

    def test_kotlin_cleanup_covers_read_lookup_error_and_job_completion(self) -> None:
        hardened = HARDENER.harden_kotlin_source(raw_kotlin_fixture())

        read = method_block(hardened, "read")
        self.assertIn("try {", read)
        self.assertIn("finally {\n                    returnValue.fill(0)", read)

        for method in ("write", "createIfAbsent", "compareAndSwap"):
            block = method_block(hardened, method)
            lifted = block.index(
                "val secretValue = FfiConverterTypeAppRelaySecretValue.lift(`value`)"
            )
            lookup = block.index("val uniffiObj = try", lifted)
            lookup_error_wipe = block.index("secretValue.fill(0)", lookup)
            make_call = block.index("val makeCall = suspend", lookup_error_wipe)
            completion_wipe = block.index(
                "{ secretValue.fill(0) },", make_call
            )
            self.assertLess(lifted, lookup)
            self.assertLess(lookup, lookup_error_wipe)
            self.assertLess(lookup_error_wipe, make_call)
            self.assertLess(make_call, completion_wipe)

        for method, argument in (
            ("signMessage", "message"),
            ("loadOrCreate", "candidate"),
        ):
            block = method_block(hardened, method)
            lifted = block.index(
                "val secretValue = "
                f"FfiConverterTypeAppRelaySecretValue.lift(`{argument}`)"
            )
            lookup = block.index("val uniffiObj = try", lifted)
            lookup_error_wipe = block.index("secretValue.fill(0)", lookup)
            make_call = block.index("val makeCall = suspend", lookup_error_wipe)
            completion_wipe = block.index(
                "{ secretValue.fill(0) },", make_call
            )
            self.assertLess(lifted, lookup)
            self.assertLess(lookup, lookup_error_wipe)
            self.assertLess(lookup_error_wipe, make_call)
            self.assertLess(make_call, completion_wipe)

        self.assertEqual(
            hardened.count("start = kotlinx.coroutines.CoroutineStart.LAZY"), 2
        )
        self.assertEqual(
            hardened.count("job.invokeOnCompletion { onCompletion?.invoke() }"),
            2,
        )
        self.assertEqual(hardened.count("job.start()"), 2)

        lazy = hardened.index("start = kotlinx.coroutines.CoroutineStart.LAZY")
        completion = hardened.index(
            "job.invokeOnCompletion { onCompletion?.invoke() }", lazy
        )
        registered = hardened.index(
            "val handle = try {", completion
        )
        dropped_callback = hardened.index(
            "uniffiOutDroppedCallback.uniffiSetValue", registered
        )
        started = hardened.index("job.start()", dropped_callback)
        self.assertLess(lazy, completion)
        self.assertLess(completion, registered)
        self.assertLess(registered, dropped_callback)
        self.assertLess(dropped_callback, started)

    def test_kotlin_registration_failures_cancel_remove_and_rethrow(self) -> None:
        hardened = HARDENER.harden_kotlin_source(raw_kotlin_fixture())

        for fallible in (False, True):
            block = kotlin_async_helper_block(hardened, fallible=fallible)
            self.assertEqual(block.count(KOTLIN_PROTECTED_REGISTRATION), 1)
            protected = block.index(KOTLIN_PROTECTED_REGISTRATION)
            insertion = block.index(
                "uniffiForeignFutureHandleMap.insert(job)", protected
            )
            insertion_cancel = block.index("job.cancel()", insertion)
            insertion_rethrow = block.index("throw error", insertion_cancel)
            publication = block.index(
                "uniffiOutDroppedCallback.uniffiSetValue", insertion_rethrow
            )
            started = block.index("job.start()", publication)
            publication_cancel = block.index("job.cancel()", started)
            removed = block.index(
                "uniffiForeignFutureHandleMap.remove(handle)", publication_cancel
            )
            publication_rethrow = block.index("throw error", removed)
            self.assertLess(insertion, insertion_cancel)
            self.assertLess(insertion_cancel, insertion_rethrow)
            self.assertLess(insertion_rethrow, publication)
            self.assertLess(publication, started)
            self.assertLess(started, publication_cancel)
            self.assertLess(publication_cancel, removed)
            self.assertLess(removed, publication_rethrow)

            catch_paths = block[block.index("val handle = try", protected) :]
            self.assertNotIn("handleSuccess(", catch_paths)
            self.assertNotIn("handleError(", catch_paths)
            self.assertNotIn("onCompletion?.invoke()", catch_paths)

    def test_kotlin_registration_upgrade_is_compatible_idempotent_and_strict(
        self,
    ) -> None:
        hardened = HARDENER.harden_kotlin_source(raw_kotlin_fixture())
        legacy = hardened.replace(
            KOTLIN_PROTECTED_REGISTRATION,
            KOTLIN_LEGACY_REGISTRATION,
        )
        self.assertEqual(legacy.count(KOTLIN_LEGACY_REGISTRATION), 2)
        self.assertEqual(HARDENER.harden_kotlin_source(legacy), hardened)
        self.assertEqual(HARDENER.harden_kotlin_source(hardened), hardened)

        drifted = hardened.replace(
            "        try {\n"
            "            uniffiForeignFutureHandleMap.remove(handle)\n",
            "        uniffiForeignFutureHandleMap.remove(handle)\n",
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "Kotlin async callback protected registration"
        ):
            HARDENER.harden_kotlin_source(drifted)

    def test_native_enrollment_capability_surface_cannot_return(self) -> None:
        for capability in ("readCapability", "manageCapability"):
            with self.assertRaisesRegex(SystemExit, "direct enrollment"):
                HARDENER.harden_swift_source(
                    raw_swift_fixture() + f"func forbidden({capability}: AppRelaySecretValue) {{}}\n"
                )
            with self.assertRaisesRegex(SystemExit, "direct enrollment"):
                HARDENER.harden_kotlin_source(
                    raw_kotlin_fixture() + f"fun forbidden(`{capability}`: AppRelaySecretValue) = Unit\n"
                )

    def test_kotlin_trailing_data_rejection_wipes_decoded_secret(self) -> None:
        hardened = HARDENER.harden_kotlin_source(raw_kotlin_fixture())
        self.assertIn(
            "if (byteBuf.hasRemaining()) {\n"
            "                secret.fill(0)\n"
            "                throw RuntimeException("
            '"junk remaining in relay-secret transfer buffer")\n'
            "            }",
            hardened,
        )

    def test_pairing_code_carriers_zeroize_direct_and_optional_lowering(self) -> None:
        swift = HARDENER.harden_swift_source(raw_swift_fixture())
        self.assertNotIn(
            "public typealias AppRemoraLinkPairingCode = Data", swift
        )
        self.assertEqual(
            swift.count(
                "public final class AppRemoraLinkPairingCode: @unchecked Sendable"
            ),
            1,
        )
        self.assertIn(
            "public static let maximumByteCount = 4_128",
            swift,
        )
        self.assertEqual(
            swift.count("throw RemoraLinkError.InvalidPairingCode"),
            3,
        )
        self.assertNotIn(
            'precondition(bytes.count <= Int(Int32.max), '
            '"pairing code is too large")',
            swift,
        )
        self.assertIn(
            "guard count <= AppRemoraLinkPairingCode.maximumByteCount",
            swift,
        )
        self.assertIn(
            "guard secretCount <= "
            "AppRemoraLinkPairingCode.maximumByteCount",
            swift,
        )
        self.assertIn(
            "public static func lower(_ value: AppRemoraLinkPairingCode) "
            "-> RustBuffer {\n"
            "        return value.lowerToRustBufferAndZeroize()",
            swift,
        )
        self.assertIn(
            "public static func lower(_ value: SwiftType) -> RustBuffer {\n"
            "        var writer = createWriter()",
            swift,
        )
        self.assertIn("            value?.zeroize()", swift)
        self.assertIn(
            "zeroizeRelaySecretMemory(baseAddress, byteCount: bytes.count)",
            swift,
        )

        kotlin = HARDENER.harden_kotlin_source(raw_kotlin_fixture())
        self.assertNotIn(
            "public typealias AppRemoraLinkPairingCode = kotlin.ByteArray",
            kotlin,
        )
        self.assertIn(
            "public class AppRemoraLinkPairingCode private constructor(",
            kotlin,
        )
        self.assertIn(
            "public const val MAXIMUM_BYTE_COUNT: kotlin.Int = 4_128",
            kotlin,
        )
        self.assertIn(
            "if (bytes.size > MAXIMUM_BYTE_COUNT) {\n"
            "                throw RemoraLinkException.InvalidPairingCode()",
            kotlin,
        )
        self.assertLess(
            kotlin.index("if (bytes.size > MAXIMUM_BYTE_COUNT)"),
            kotlin.index("AppRemoraLinkPairingCode(bytes.copyOf())"),
        )
        self.assertLess(
            kotlin.index(
                "if (length < 0 || length > "
                "AppRemoraLinkPairingCode.MAXIMUM_BYTE_COUNT)"
            ),
            kotlin.index("val bytes = kotlin.ByteArray(length)"),
        )
        self.assertNotIn(
            "public typealias FfiConverterTypeAppRemoraLinkPairingCode = "
            "FfiConverterByteArray",
            kotlin,
        )
        self.assertIn(
            "public object FfiConverterTypeAppRemoraLinkPairingCode: "
            "FfiConverterRustBuffer<AppRemoraLinkPairingCode>",
            kotlin,
        )
        self.assertIn(
            "throw RuntimeException("
            '"junk remaining in pairing-code transfer buffer")',
            kotlin,
        )
        self.assertIn(
            "override fun lower(value: AppRemoraLinkPairingCode?): "
            "RustBuffer.ByValue =\n"
            "        try {\n"
            "            lowerIntoRustBuffer(value)\n"
            "        } finally {\n"
            "            value?.zeroize()",
            kotlin,
        )

    def test_pairing_code_converter_has_one_documentation_attribute(self) -> None:
        swift = HARDENER.harden_swift_source(raw_swift_fixture())
        converter_documentation = """#if swift(>=5.8)
@_documentation(visibility: private)
#endif
public struct FfiConverterTypeAppRemoraLinkPairingCode: FfiConverter"""
        duplicated_documentation = converter_documentation.replace(
            "public struct",
            "#if swift(>=5.8)\n"
            "@_documentation(visibility: private)\n"
            "#endif\n"
            "public struct",
        )

        self.assertEqual(swift.count(converter_documentation), 1)
        self.assertNotIn(duplicated_documentation, swift)

        duplicated = swift.replace(
            converter_documentation,
            duplicated_documentation,
            1,
        )
        with self.assertRaisesRegex(
            SystemExit,
            "Swift duplicate pairing-code converter documentation",
        ):
            HARDENER.harden_swift_source(duplicated)

    def test_pairing_code_template_drift_fails_closed(self) -> None:
        swift_without_optional = raw_swift_fixture().replace(
            "fileprivate struct FfiConverterOptionTypeAppRemoraLinkPairingCode: "
            "FfiConverterRustBuffer {",
            "fileprivate struct FfiConverterOptionTypeUnexpected: "
            "FfiConverterRustBuffer {",
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "Swift optional pairing-code converter"
        ):
            HARDENER.harden_swift_source(swift_without_optional)

        kotlin_without_optional = raw_kotlin_fixture().replace(
            "public object FfiConverterOptionalTypeAppRemoraLinkPairingCode: "
            "FfiConverterRustBuffer<AppRemoraLinkPairingCode?> {",
            "public object FfiConverterOptionalTypeUnexpected: "
            "FfiConverterRustBuffer<AppRemoraLinkPairingCode?> {",
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "Kotlin optional pairing-code converter"
        ):
            HARDENER.harden_kotlin_source(kotlin_without_optional)

    def test_swift_remora_link_futures_are_cancellable_and_ordinary_async_is_unchanged(
        self,
    ) -> None:
        raw = raw_swift_fixture()
        hardened = HARDENER.harden_swift_source(raw)

        self.assertEqual(hardened.count(SWIFT_ASYNC_HELPER), 1)
        self.assertEqual(
            swift_app_client_method(hardened, "ordinaryOperation").count(
                "uniffiRustCallAsync("
            ),
            1,
        )
        for method in (
            "inspectRemoraLinkCode",
            "acceptRemoraLinkOffer",
            "awaitRemoraLinkPairing",
        ):
            block = swift_app_client_method(hardened, method)
            self.assertEqual(block.count("uniffiRustCallAsyncCancellable("), 1, method)
            self.assertEqual(
                block.count(
                    "cancelFunc: "
                    "ffi_codex_mobile_client_rust_future_cancel_rust_buffer"
                ),
                1,
                method,
            )
            self.assertEqual(
                block.count(
                    "freeFunc: "
                    "ffi_codex_mobile_client_rust_future_free_rust_buffer"
                ),
                1,
                method,
            )

        self.assertEqual(
            hardened.count(
                "fileprivate final class "
                "UniffiRustFutureCancellationCoordinator: @unchecked Sendable"
            ),
            1,
        )
        self.assertEqual(hardened.count("throw CancellationError()"), 1)
        self.assertEqual(HARDENER.harden_swift_source(hardened), hardened)

    def test_swift_cancellable_future_transform_fails_closed_on_template_drift(
        self,
    ) -> None:
        helper_drift = raw_swift_fixture().replace(
            "    var pollResult: Int8;\n",
            "    var pollResult: Int8\n",
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "Swift UniFFI async helper template"
        ):
            HARDENER.harden_swift_source(helper_drift)

        target_drift = raw_swift_fixture().replace(
            SWIFT_ACCEPT_METHOD,
            SWIFT_ACCEPT_METHOD.replace(
                "freeFunc: ffi_codex_mobile_client_rust_future_free_rust_buffer",
                "freeFunc: ffi_codex_mobile_client_rust_future_free_rust_buffer_v2",
                1,
            ),
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "Swift acceptRemoraLinkOffer cancellable future"
        ):
            HARDENER.harden_swift_source(target_drift)

        hardened = HARDENER.harden_swift_source(raw_swift_fixture())
        support_drift = hardened.replace(
            "    private var freed = false\n",
            "    private var freed: Bool = false\n",
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "Swift cancellable UniFFI async support"
        ):
            HARDENER.harden_swift_source(support_drift)

        hardened_drift = hardened.replace(
            "cancelFunc: ffi_codex_mobile_client_rust_future_cancel_rust_buffer",
            "cancelFunc: ffi_codex_mobile_client_rust_future_cancel_void",
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "Swift inspectRemoraLinkCode cancellable future"
        ):
            HARDENER.harden_swift_source(hardened_drift)

    def test_swift_raw_and_hardened_sources_are_idempotent_and_wipe_all_exits(
        self,
    ) -> None:
        hardened = HARDENER.harden_swift_source(raw_swift_fixture())

        self.assertEqual(HARDENER.harden_swift_source(hardened), hardened)
        self.assertEqual(hardened.count("defer { returnValue.zeroize() }"), 3)
        self.assertEqual(hardened.count("defer { secretValue.zeroize() }"), 5)
        self.assertEqual(
            hardened.count(
                "let secretValue = try FfiConverterTypeAppRelaySecretValue_lift(value)\n"
                "                defer { secretValue.zeroize() }\n"
                "                guard let uniffiObj"
            ),
            3,
        )
        self.assertEqual(
            hardened.count(
                "let secretValue = try "
                "FfiConverterTypeAppRelaySecretValue_lift(message)"
            ),
            1,
        )
        self.assertEqual(
            hardened.count(
                "let secretValue = try "
                "FfiConverterTypeAppRelaySecretValue_lift(candidate)"
            ),
            1,
        )
        self.assertEqual(hardened.count("if returnValue === secretValue"), 2)
        self.assertEqual(
            hardened.count("return value.lowerToRustBufferAndZeroize()"), 2
        )
        self.assertEqual(
            hardened.count("defer { value.zeroizeAndDeallocateSecret() }"), 2
        )

    def test_swift_secret_result_callbacks_are_hardened_per_method(self) -> None:
        unrelated = "\n// Unrelated generated callback.\n" + swift_secret_success_callback()
        hardened = HARDENER.harden_swift_source(raw_swift_fixture() + unrelated)

        self.assertEqual(hardened.count(swift_secret_success_callback()), 1)
        self.assertTrue(hardened.endswith(unrelated))
        for backend_name, method in (
            ("AppRelaySecretBackend", "read"),
            ("AppRemoraLinkDeviceKeyBackend", "signMessage"),
            ("AppRemoraLinkTransportIdentityBackend", "loadOrCreate"),
        ):
            block = HARDENER.swift_callback_method(
                hardened, backend_name, method
            )
            self.assertEqual(
                block.count(hardened_swift_secret_success_callback()), 1
            )
            self.assertEqual(block.count("defer { returnValue.zeroize() }"), 1)

    def test_swift_unrelated_callback_cannot_mask_target_method_drift(self) -> None:
        raw = raw_swift_fixture()
        method_start = raw.index("        signMessage: { (")
        callback_start = raw.index(swift_secret_success_callback(), method_start)
        callback_end = callback_start + len(swift_secret_success_callback())
        drifted_callback = swift_secret_success_callback().replace(
            "callStatus: RustCallStatus()",
            "callStatus: RustCallStatus(code: 0)",
            1,
        )
        substituted = (
            raw[:callback_start]
            + drifted_callback
            + raw[callback_end:]
            + "\n// Unrelated generated callback.\n"
            + swift_secret_success_callback()
        )
        self.assertEqual(substituted.count(swift_secret_success_callback()), 3)

        with self.assertRaisesRegex(
            SystemExit, "Swift AppRemoraLinkDeviceKeyBackend.signMessage"
        ):
            HARDENER.harden_swift_source(substituted)

    def test_raw_template_drift_fails_closed(self) -> None:
        kotlin_without_delete = raw_kotlin_fixture().replace(
            callback("delete", ASYNC_CALL), "", 1
        )
        with self.assertRaisesRegex(
            SystemExit, "Kotlin relay-secret callbacks"
        ):
            HARDENER.harden_kotlin_source(kotlin_without_delete)

        kotlin_without_transport = raw_kotlin_fixture().replace(
            "internal object uniffiCallbackInterfaceAppRemoraLinkTransportIdentityBackend {",
            "internal object uniffiCallbackInterfaceUnexpectedBackend {",
            1,
        )
        with self.assertRaisesRegex(
            SystemExit, "AppRemoraLinkTransportIdentityBackend"
        ):
            HARDENER.harden_kotlin_source(kotlin_without_transport)

        swift_without_converter = raw_swift_fixture().replace(
            "public typealias AppRelaySecretValue = Data\n", "", 1
        )
        with self.assertRaisesRegex(SystemExit, "Swift reference secret carrier"):
            HARDENER.harden_swift_source(swift_without_converter)

    def test_rehardening_rejects_cleanup_in_secret_free_callback(self) -> None:
        hardened = HARDENER.harden_kotlin_source(raw_kotlin_fixture())
        start, end = HARDENER.kotlin_secret_backend_method_span(
            hardened, "compareAndTombstone"
        )
        bad_block = hardened[start:end].replace(
            ASYNC_CALL,
            ASYNC_CALL.replace(
                "                uniffiOutDroppedCallback\n",
                "                uniffiOutDroppedCallback,\n"
                "                { secretValue.fill(0) },\n",
            ),
            1,
        )
        misplaced = hardened[:start] + bad_block + hardened[end:]

        with self.assertRaisesRegex(
            SystemExit, "compareAndTombstone absence of nonexistent secret cleanup"
        ):
            HARDENER.harden_kotlin_source(misplaced)


if __name__ == "__main__":
    unittest.main()
