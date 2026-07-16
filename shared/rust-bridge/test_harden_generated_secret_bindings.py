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


def raw_swift_fixture() -> str:
    return """import Foundation

extension RustBuffer {
    func deallocate() {
        try! rustCall { ffi_codex_mobile_client_rustbuffer_free(self, $0) }
    }
}

public typealias AppRelaySecretValue = Data

enum FfiConverterTypeAppRelaySecretValue {
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

            let uniffiHandleSuccess = { (returnValue: AppRelaySecretValue) in
                uniffiFutureCallback(
                    uniffiCallbackData,
                    UniffiForeignFutureResultRustBuffer(
                        returnValue: FfiConverterTypeAppRelaySecretValue_lower(returnValue),
                        callStatus: RustCallStatus()
                    )
                )
            }

""" + swift_value_callback(
        "write", "AppRelaySecretWriteOutcome"
    ) + swift_value_callback(
        "createIfAbsent", "AppRelaySecretCreateOutcome"
    ) + """            let makeCall = {
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

func pushOne(token: AppRelaySecretValue) {}
func pushTwo(token: AppRelaySecretValue) {}
func enrollOne(readCapability: AppRelaySecretValue, manageCapability: AppRelaySecretValue) {}
func enrollTwo(readCapability: AppRelaySecretValue, manageCapability: AppRelaySecretValue) {}
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

    return """public typealias AppRelaySecretValue = kotlin.ByteArray
public typealias FfiConverterTypeAppRelaySecretValue = FfiConverterByteArray

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

fun pushOne(`token`: AppRelaySecretValue) = Unit
fun pushTwo(`token`: AppRelaySecretValue) = Unit
fun enrollOne(`readCapability`: AppRelaySecretValue, `manageCapability`: AppRelaySecretValue) = Unit
fun enrollTwo(`readCapability`: AppRelaySecretValue, `manageCapability`: AppRelaySecretValue) = Unit
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

        lazy = hardened.index("start = kotlinx.coroutines.CoroutineStart.LAZY")
        completion = hardened.index(
            "job.invokeOnCompletion { onCompletion?.invoke() }", lazy
        )
        registered = hardened.index(
            "val handle = uniffiForeignFutureHandleMap.insert(job)", completion
        )
        dropped_callback = hardened.index(
            "uniffiOutDroppedCallback.uniffiSetValue", registered
        )
        started = hardened.index("job.start()", dropped_callback)
        self.assertLess(lazy, completion)
        self.assertLess(completion, registered)
        self.assertLess(registered, dropped_callback)
        self.assertLess(dropped_callback, started)

    def test_swift_raw_and_hardened_sources_are_idempotent_and_wipe_all_exits(
        self,
    ) -> None:
        hardened = HARDENER.harden_swift_source(raw_swift_fixture())

        self.assertEqual(HARDENER.harden_swift_source(hardened), hardened)
        self.assertEqual(hardened.count("defer { returnValue.zeroize() }"), 1)
        self.assertEqual(
            hardened.count(
                "let secretValue = try FfiConverterTypeAppRelaySecretValue_lift(value)\n"
                "                defer { secretValue.zeroize() }\n"
                "                guard let uniffiObj"
            ),
            3,
        )
        self.assertEqual(
            hardened.count("return value.lowerToRustBufferAndZeroize()"), 1
        )
        self.assertEqual(
            hardened.count("defer { value.zeroizeAndDeallocateSecret() }"), 1
        )

    def test_raw_template_drift_fails_closed(self) -> None:
        kotlin_without_delete = raw_kotlin_fixture().replace(
            callback("delete", ASYNC_CALL), "", 1
        )
        with self.assertRaisesRegex(
            SystemExit, "Kotlin relay-secret callbacks"
        ):
            HARDENER.harden_kotlin_source(kotlin_without_delete)

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
