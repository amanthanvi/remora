#!/usr/bin/env python3
"""Harden generated UniFFI relay-secret transfers and fail on template drift."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def replace_once(source: str, old: str, new: str, label: str) -> str:
    count = source.count(old)
    if count != 1:
        raise SystemExit(
            f"error: generated binding drift for {label}: expected 1 match, found {count}"
        )
    return source.replace(old, new, 1)


def require_exact(source: str, needle: str, expected: int, label: str) -> None:
    count = source.count(needle)
    if count != expected:
        raise SystemExit(
            f"error: generated binding drift for {label}: "
            f"expected {expected} matches, found {count}"
        )


def harden_swift(path: Path) -> None:
    source = path.read_text()
    source = replace_once(
        source,
        "import Foundation\n",
        """import Foundation
import Darwin

@inline(never)
fileprivate func zeroizeRelaySecretMemory(
    _ pointer: UnsafeMutableRawPointer,
    byteCount: Int
) {
    guard byteCount > 0 else { return }
    let status = memset_s(pointer, byteCount, 0, byteCount)
    if status != 0 {
        fatalError("failed to zero relay-secret memory")
    }
}
""",
        "Swift secure-zeroization import",
    )
    source = replace_once(
        source,
        """    func deallocate() {
        try! rustCall { ffi_codex_mobile_client_rustbuffer_free(self, $0) }
    }
""",
        """    func deallocate() {
        try! rustCall { ffi_codex_mobile_client_rustbuffer_free(self, $0) }
    }

    // Secret-only transfer cleanup. Wipe Rust-owned bytes before returning
    // their allocation to Rust; ordinary generated buffers keep the standard
    // deallocation path above.
    func zeroizeAndDeallocateSecret() {
        if let data = self.data, self.len > 0 {
            zeroizeRelaySecretMemory(
                UnsafeMutableRawPointer(data),
                byteCount: Int(self.len)
            )
        }
        deallocate()
    }
""",
        "Swift RustBuffer secret cleanup",
    )
    source = replace_once(
        source,
        "public typealias AppRelaySecretValue = Data\n",
        """/// A single-allocation, reference-semantic relay-secret carrier.
///
/// Copies made by assigning this object remain aliases of the same mutable
/// allocation. UniFFI lowering and native callback completion zero that
/// allocation, so every retained alias observes only zeroes afterwards.
/// Source buffers passed to an initializer remain owned by the caller and
/// should be cleared by the caller when no longer needed.
public final class AppRelaySecretValue: @unchecked Sendable {
    private static let lengthPrefixCount = MemoryLayout<Int32>.size

    private let storage: UnsafeMutableRawPointer
    private let lock = NSLock()
    public let count: Int

    public init(copying bytes: UnsafeRawBufferPointer) {
        precondition(bytes.count <= Int(Int32.max), "relay secret is too large")
        self.count = bytes.count
        self.storage = Self.allocateStorage(copying: bytes)
    }

    public init(copying data: Data) {
        precondition(data.count <= Int(Int32.max), "relay secret is too large")
        self.count = data.count
        self.storage = data.withUnsafeBytes { Self.allocateStorage(copying: $0) }
    }

    public init(copying bytes: [UInt8]) {
        precondition(bytes.count <= Int(Int32.max), "relay secret is too large")
        self.count = bytes.count
        self.storage = bytes.withUnsafeBytes { Self.allocateStorage(copying: $0) }
    }

    deinit {
        lock.lock()
        let byteCount = Self.lengthPrefixCount + count
        zeroizeRelaySecretMemory(storage, byteCount: byteCount)
        storage.deallocate()
        lock.unlock()
    }

    /// Borrows the secret bytes without manufacturing a Data or Array copy.
    /// The closure must not re-enter this carrier.
    public func withUnsafeBytes<Result>(
        _ body: (UnsafeRawBufferPointer) throws -> Result
    ) rethrows -> Result {
        lock.lock()
        defer { lock.unlock() }
        return try body(secretBytesLocked())
    }

    /// Clears the shared secret allocation. All retained aliases observe zeroes.
    public func zeroize() {
        lock.lock()
        defer { lock.unlock() }
        zeroizeSecretLocked()
    }

    fileprivate func lowerToRustBufferAndZeroize() -> RustBuffer {
        lock.lock()
        defer {
            zeroizeSecretLocked()
            lock.unlock()
        }
        let encoded = UnsafeRawBufferPointer(
            start: storage,
            count: Self.lengthPrefixCount + count
        ).bindMemory(to: UInt8.self)
        return RustBuffer.from(encoded)
    }

    private static func allocateStorage(
        copying bytes: UnsafeRawBufferPointer
    ) -> UnsafeMutableRawPointer {
        let byteCount = lengthPrefixCount + bytes.count
        let storage = UnsafeMutableRawPointer.allocate(
            byteCount: byteCount,
            alignment: MemoryLayout<UInt32>.alignment
        )
        let encoded = storage.assumingMemoryBound(to: UInt8.self)
        let length = UInt32(bytes.count)
        encoded[0] = UInt8(truncatingIfNeeded: length >> 24)
        encoded[1] = UInt8(truncatingIfNeeded: length >> 16)
        encoded[2] = UInt8(truncatingIfNeeded: length >> 8)
        encoded[3] = UInt8(truncatingIfNeeded: length)
        if let source = bytes.baseAddress, bytes.count > 0 {
            storage.advanced(by: lengthPrefixCount).copyMemory(
                from: source,
                byteCount: bytes.count
            )
        }
        return storage
    }

    private func secretBytesLocked() -> UnsafeRawBufferPointer {
        UnsafeRawBufferPointer(
            start: storage.advanced(by: Self.lengthPrefixCount),
            count: count
        )
    }

    private func zeroizeSecretLocked() {
        guard count > 0 else { return }
        let secret = storage.advanced(by: Self.lengthPrefixCount)
        zeroizeRelaySecretMemory(secret, byteCount: count)
    }
}
""",
        "Swift reference secret carrier",
    )
    source = replace_once(
        source,
        """    public static func read(from buf: inout (data: Data, offset: Data.Index)) throws -> AppRelaySecretValue {
        return try FfiConverterData.read(from: &buf)
    }
""",
        """    public static func read(from buf: inout (data: Data, offset: Data.Index)) throws -> AppRelaySecretValue {
        let length: Int32 = try readInt(&buf)
        guard length >= 0 else {
            throw UniffiInternalError.bufferOverflow
        }
        let count = Int(length)
        guard buf.offset <= buf.data.count, count <= buf.data.count - buf.offset else {
            throw UniffiInternalError.bufferOverflow
        }
        let range = buf.offset..<(buf.offset + count)
        let secret = buf.data.withUnsafeBytes { bytes in
            AppRelaySecretValue(
                copying: UnsafeRawBufferPointer(rebasing: bytes[range])
            )
        }
        buf.offset = range.upperBound
        return secret
    }
""",
        "Swift secret converter nested read",
    )
    source = replace_once(
        source,
        """    public static func write(_ value: AppRelaySecretValue, into buf: inout [UInt8]) {
        return FfiConverterData.write(value, into: &buf)
    }
""",
        """    public static func write(_ value: AppRelaySecretValue, into buf: inout [UInt8]) {
        writeInt(&buf, Int32(value.count))
        value.withUnsafeBytes { writeBytes(&buf, $0) }
    }
""",
        "Swift secret converter nested write",
    )
    source = replace_once(
        source,
        """    public static func lift(_ value: RustBuffer) throws -> AppRelaySecretValue {
        return try FfiConverterData.lift(value)
    }
""",
        """    public static func lift(_ value: RustBuffer) throws -> AppRelaySecretValue {
        defer { value.zeroizeAndDeallocateSecret() }
        guard value.len >= Int32(MemoryLayout<Int32>.size), let data = value.data else {
            throw UniffiInternalError.bufferOverflow
        }
        let encodedCount = Int(value.len)
        let encoded = UnsafeRawBufferPointer(start: data, count: encodedCount)
        let bytes = encoded.bindMemory(to: UInt8.self)
        let secretCount = Int(
            UInt32(bytes[0]) << 24 |
            UInt32(bytes[1]) << 16 |
            UInt32(bytes[2]) << 8 |
            UInt32(bytes[3])
        )
        guard secretCount <= Int(Int32.max) else {
            throw UniffiInternalError.bufferOverflow
        }
        let expectedCount = MemoryLayout<Int32>.size + secretCount
        guard encodedCount >= expectedCount else {
            throw UniffiInternalError.bufferOverflow
        }
        guard encodedCount == expectedCount else {
            throw UniffiInternalError.incompleteData
        }
        return AppRelaySecretValue(
            copying: UnsafeRawBufferPointer(
                start: data.advanced(by: MemoryLayout<Int32>.size),
                count: secretCount
            )
        )
    }
""",
        "Swift secret converter direct lift",
    )
    source = replace_once(
        source,
        """    public static func lower(_ value: AppRelaySecretValue) -> RustBuffer {
        return FfiConverterData.lower(value)
    }
""",
        """    public static func lower(_ value: AppRelaySecretValue) -> RustBuffer {
        return value.lowerToRustBufferAndZeroize()
    }
""",
        "Swift secret converter direct lowering",
    )
    source = replace_once(
        source,
        """            let uniffiHandleSuccess = { (returnValue: AppRelaySecretValue) in
                uniffiFutureCallback(
                    uniffiCallbackData,
                    UniffiForeignFutureResultRustBuffer(
                        returnValue: FfiConverterTypeAppRelaySecretValue_lower(returnValue),
                        callStatus: RustCallStatus()
                    )
                )
            }
""",
        """            let uniffiHandleSuccess = { (returnValue: AppRelaySecretValue) in
                defer { returnValue.zeroize() }
                uniffiFutureCallback(
                    uniffiCallbackData,
                    UniffiForeignFutureResultRustBuffer(
                        returnValue: FfiConverterTypeAppRelaySecretValue_lower(returnValue),
                        callStatus: RustCallStatus()
                    )
                )
            }
""",
        "Swift read secret success callback",
    )
    for method, outcome in (
        ("write", "AppRelaySecretWriteOutcome"),
        ("createIfAbsent", "AppRelaySecretCreateOutcome"),
    ):
        source = replace_once(
            source,
            f"""            let makeCall = {{
                () async throws -> {outcome} in
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRelaySecretBackend.handleMap.get(handle: uniffiHandle) else {{
                    throw UniffiInternalError.unexpectedStaleHandle
                }}
                return await uniffiObj.{method}(
                     alias: try FfiConverterString.lift(alias),
                     value: try FfiConverterTypeAppRelaySecretValue_lift(value)
                )
            }}
""",
            f"""            let makeCall = {{
                () async throws -> {outcome} in
                let secretValue = try FfiConverterTypeAppRelaySecretValue_lift(value)
                defer {{ secretValue.zeroize() }}
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRelaySecretBackend.handleMap.get(handle: uniffiHandle) else {{
                    throw UniffiInternalError.unexpectedStaleHandle
                }}
                return await uniffiObj.{method}(
                     alias: try FfiConverterString.lift(alias),
                     value: secretValue
                )
            }}
""",
            f"Swift {method} secret callback",
        )
    source = replace_once(
        source,
        """            let makeCall = {
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
""",
        """            let makeCall = {
                () async throws -> AppRelaySecretCasOutcome in
                let secretValue = try FfiConverterTypeAppRelaySecretValue_lift(value)
                defer { secretValue.zeroize() }
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRelaySecretBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                return await uniffiObj.compareAndSwap(
                     alias: try FfiConverterString.lift(alias),
                     expectedRevision: try FfiConverterOptionUInt64.lift(expectedRevision),
                     replacementRevision: try FfiConverterUInt64.lift(replacementRevision),
                     value: secretValue
                )
            }
""",
        "Swift compareAndSwap secret callback",
    )
    require_exact(
        source,
        "token: AppRelaySecretValue",
        2,
        "Swift direct push-token argument",
    )
    require_exact(
        source,
        "readCapability: AppRelaySecretValue",
        2,
        "Swift direct enrollment read-capability argument",
    )
    require_exact(
        source,
        "manageCapability: AppRelaySecretValue",
        2,
        "Swift direct enrollment manage-capability argument",
    )
    require_exact(
        source,
        "public final class AppRelaySecretValue: @unchecked Sendable",
        1,
        "Swift reference secret carrier",
    )
    require_exact(
        source,
        "fileprivate func zeroizeRelaySecretMemory(",
        1,
        "Swift non-elidable zeroization helper",
    )
    require_exact(
        source,
        "let status = memset_s(pointer, byteCount, 0, byteCount)",
        1,
        "Swift secure memset implementation",
    )
    require_exact(
        source,
        "return value.lowerToRustBufferAndZeroize()",
        1,
        "Swift direct zeroizing lower",
    )
    require_exact(
        source,
        "FfiConverterTypeAppRelaySecretValue.read(",
        0,
        "Swift absence of nested secret reads",
    )
    require_exact(
        source,
        "FfiConverterTypeAppRelaySecretValue.write(",
        0,
        "Swift absence of nested secret writes",
    )
    require_exact(
        source,
        "defer { secretValue.zeroize() }",
        3,
        "Swift callback secret completion wipe",
    )
    require_exact(
        source,
        "defer { returnValue.zeroize() }",
        1,
        "Swift read-result completion wipe",
    )
    path.write_text(source)


def verify_swift_runtime(path: Path, library_dir: Path) -> None:
    verifier = """import Foundation

let expected: [UInt8] = [0x52, 0x65, 0x6d, 0x6f, 0x72, 0x61]
let carrier = AppRelaySecretValue(copying: expected)
let retainedAlias = carrier
precondition(retainedAlias === carrier)

let lowered = FfiConverterTypeAppRelaySecretValue_lower(carrier)
precondition(retainedAlias.withUnsafeBytes { bytes in
    bytes.count == expected.count && bytes.allSatisfy { $0 == 0 }
}, "retained alias did not observe zeroization after lowering")

let lifted = try FfiConverterTypeAppRelaySecretValue_lift(lowered)
precondition(lifted.withUnsafeBytes { bytes in
    guard bytes.count == expected.count else { return false }
    for index in expected.indices where bytes[index] != expected[index] {
        return false
    }
    return true
}, "direct lift/lower did not preserve the transferred secret")

let liftedAlias = lifted
lifted.zeroize()
precondition(liftedAlias.withUnsafeBytes { $0.allSatisfy { $0 == 0 } })
print("Swift relay-secret carrier runtime verification passed")
"""
    with tempfile.TemporaryDirectory(prefix="remora-secret-swift-") as temp_dir:
        temp = Path(temp_dir)
        main_path = temp / "main.swift"
        executable = temp / "relay-secret-runtime-test"
        main_path.write_text(verifier)
        compile_result = subprocess.run(
            [
                "xcrun",
                "swiftc",
                "-I",
                str(path.parent),
                "-L",
                str(library_dir),
                "-lcodex_mobile_client",
                "-Xlinker",
                "-rpath",
                "-Xlinker",
                str(library_dir),
                str(path),
                str(main_path),
                "-o",
                str(executable),
            ],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode != 0:
            raise SystemExit(
                "error: Swift relay-secret runtime verifier failed to compile:\n"
                f"{compile_result.stdout}{compile_result.stderr}"
            )
        runtime_environment = os.environ.copy()
        runtime_environment["DYLD_LIBRARY_PATH"] = str(library_dir)
        run_result = subprocess.run(
            [str(executable)],
            capture_output=True,
            text=True,
            env=runtime_environment,
        )
        if run_result.returncode != 0:
            raise SystemExit(
                "error: Swift relay-secret runtime verifier failed:\n"
                f"{run_result.stdout}{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def harden_kotlin(path: Path) -> None:
    source = path.read_text()
    source = replace_once(
        source,
        """public typealias AppRelaySecretValue = kotlin.ByteArray
public typealias FfiConverterTypeAppRelaySecretValue = FfiConverterByteArray
""",
        """public typealias AppRelaySecretValue = kotlin.ByteArray
public object FfiConverterTypeAppRelaySecretValue: FfiConverterRustBuffer<AppRelaySecretValue> {
    override fun lift(value: RustBuffer.ByValue): AppRelaySecretValue {
        try {
            val byteBuf = value.asByteBuffer()
                ?: throw RuntimeException("null relay-secret transfer buffer")
            val secret = read(byteBuf)
            if (byteBuf.hasRemaining()) {
                throw RuntimeException("junk remaining in relay-secret transfer buffer")
            }
            return secret
        } finally {
            value.data?.setMemory(0, value.len, 0.toByte())
            RustBuffer.free(value)
        }
    }

    override fun read(buf: ByteBuffer): AppRelaySecretValue = FfiConverterByteArray.read(buf)

    override fun lower(value: AppRelaySecretValue): RustBuffer.ByValue =
        try {
            lowerIntoRustBuffer(value)
        } finally {
            value.fill(0)
        }

    override fun allocationSize(value: AppRelaySecretValue): ULong =
        FfiConverterByteArray.allocationSize(value)

    override fun write(value: AppRelaySecretValue, buf: ByteBuffer) =
        FfiConverterByteArray.write(value, buf)
}
""",
        "Kotlin secret converter",
    )
    source = replace_once(
        source,
        """internal inline fun<T> uniffiTraitInterfaceCallAsync(
    crossinline makeCall: suspend () -> T,
    crossinline handleSuccess: (T) -> Unit,
    crossinline handleError: (UniffiRustCallStatus.ByValue) -> Unit,
    uniffiOutDroppedCallback: UniffiForeignFutureDroppedCallbackStruct,
) {
""",
        """internal inline fun<T> uniffiTraitInterfaceCallAsync(
    crossinline makeCall: suspend () -> T,
    crossinline handleSuccess: (T) -> Unit,
    crossinline handleError: (UniffiRustCallStatus.ByValue) -> Unit,
    uniffiOutDroppedCallback: UniffiForeignFutureDroppedCallbackStruct,
    noinline onCompletion: (() -> Unit)? = null,
) {
""",
        "Kotlin async callback completion parameter",
    )
    source = replace_once(
        source,
        """    val job = GlobalScope.launch coroutineBlock@ {
        // Note: it's important we call either `handleSuccess` or `handleError` exactly once.  Each
""",
        """    val job = GlobalScope.launch(
        start = kotlinx.coroutines.CoroutineStart.LAZY,
    ) coroutineBlock@ {
        // Note: it's important we call either `handleSuccess` or `handleError` exactly once.  Each
""",
        "Kotlin pre-dispatch lazy callback job",
    )
    source = replace_once(
        source,
        """        handleSuccess(callResult)
    }
    val handle = uniffiForeignFutureHandleMap.insert(job)
    uniffiOutDroppedCallback.uniffiSetValue(UniffiForeignFutureDroppedCallbackStruct(handle, uniffiForeignFutureDroppedCallbackImpl))
}

internal inline fun<T, reified E: Throwable> uniffiTraitInterfaceCallAsyncWithError(
""",
        """        handleSuccess(callResult)
    }
    job.invokeOnCompletion { onCompletion?.invoke() }
    val handle = uniffiForeignFutureHandleMap.insert(job)
    uniffiOutDroppedCallback.uniffiSetValue(UniffiForeignFutureDroppedCallbackStruct(handle, uniffiForeignFutureDroppedCallbackImpl))
    job.start()
}

internal inline fun<T, reified E: Throwable> uniffiTraitInterfaceCallAsyncWithError(
""",
        "Kotlin async callback completion hook",
    )
    source = replace_once(
        source,
        """            val uniffiHandleSuccess = { returnValue: AppRelaySecretValue ->
                val uniffiResult = UniffiForeignFutureResultRustBuffer.UniffiByValue(
                    FfiConverterTypeAppRelaySecretValue.lower(returnValue),
                    UniffiRustCallStatus.ByValue()
                )
                uniffiResult.write()
                uniffiFutureCallback.callback(uniffiCallbackData, uniffiResult)
            }
""",
        """            val uniffiHandleSuccess = { returnValue: AppRelaySecretValue ->
                try {
                    val uniffiResult = UniffiForeignFutureResultRustBuffer.UniffiByValue(
                        FfiConverterTypeAppRelaySecretValue.lower(returnValue),
                        UniffiRustCallStatus.ByValue()
                    )
                    uniffiResult.write()
                    uniffiFutureCallback.callback(uniffiCallbackData, uniffiResult)
                } finally {
                    returnValue.fill(0)
                }
            }
""",
        "Kotlin read secret success callback",
    )
    for method in ("write", "createIfAbsent"):
        source = replace_once(
            source,
            f"""            val uniffiObj = FfiConverterTypeAppRelaySecretBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend {{ ->
                uniffiObj.`{method}`(
                    FfiConverterString.lift(`alias`),
                    FfiConverterTypeAppRelaySecretValue.lift(`value`),
                )
            }}
""",
            f"""            val secretValue = FfiConverterTypeAppRelaySecretValue.lift(`value`)
            val uniffiObj = try {{
                FfiConverterTypeAppRelaySecretBackend.handleMap.get(uniffiHandle)
            }} catch (error: Throwable) {{
                secretValue.fill(0)
                throw error
            }}
            val makeCall = suspend {{ ->
                uniffiObj.`{method}`(
                    FfiConverterString.lift(`alias`),
                    secretValue,
                )
            }}
""",
            f"Kotlin {method} secret callback",
        )
        next_method = "createIfAbsent" if method == "write" else "revision"
        source = replace_once(
            source,
            f"""            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback
            )
        }}
    }}
    internal object `{next_method}`:""",
            f"""            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback,
                {{ secretValue.fill(0) }},
            )
        }}
    }}
    internal object `{next_method}`:""",
            f"Kotlin {method} secret completion hook",
        )
    source = replace_once(
        source,
        """            val uniffiObj = FfiConverterTypeAppRelaySecretBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`compareAndSwap`(
                    FfiConverterString.lift(`alias`),
                    FfiConverterOptionalULong.lift(`expectedRevision`),
                    FfiConverterULong.lift(`replacementRevision`),
                    FfiConverterTypeAppRelaySecretValue.lift(`value`),
                )
            }
""",
        """            val secretValue = FfiConverterTypeAppRelaySecretValue.lift(`value`)
            val uniffiObj = try {
                FfiConverterTypeAppRelaySecretBackend.handleMap.get(uniffiHandle)
            } catch (error: Throwable) {
                secretValue.fill(0)
                throw error
            }
            val makeCall = suspend { ->
                uniffiObj.`compareAndSwap`(
                    FfiConverterString.lift(`alias`),
                    FfiConverterOptionalULong.lift(`expectedRevision`),
                    FfiConverterULong.lift(`replacementRevision`),
                    secretValue,
                )
            }
""",
        "Kotlin compareAndSwap secret callback",
    )
    source = replace_once(
        source,
        """            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback
            )
        }
    }
    internal object `delete`:""",
        """            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback,
                { secretValue.fill(0) },
            )
        }
    }
    internal object `delete`:""",
        "Kotlin compareAndSwap secret completion hook",
    )
    require_exact(
        source,
        "`token`: AppRelaySecretValue",
        2,
        "Kotlin direct push-token argument",
    )
    require_exact(
        source,
        "`readCapability`: AppRelaySecretValue",
        2,
        "Kotlin direct enrollment read-capability argument",
    )
    require_exact(
        source,
        "`manageCapability`: AppRelaySecretValue",
        2,
        "Kotlin direct enrollment manage-capability argument",
    )
    path.write_text(source)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--swift", type=Path)
    parser.add_argument("--kotlin", type=Path)
    parser.add_argument("--verify-swift-runtime", action="store_true")
    parser.add_argument("--swift-library-dir", type=Path)
    args = parser.parse_args()
    if args.swift is None and args.kotlin is None:
        parser.error("at least one generated binding path is required")
    if args.verify_swift_runtime and args.swift is None:
        parser.error("--verify-swift-runtime requires --swift")
    if args.verify_swift_runtime and args.swift_library_dir is None:
        parser.error("--verify-swift-runtime requires --swift-library-dir")
    if args.swift_library_dir is not None and not args.verify_swift_runtime:
        parser.error("--swift-library-dir requires --verify-swift-runtime")
    if args.swift is not None:
        harden_swift(args.swift)
    if args.kotlin is not None:
        harden_kotlin(args.kotlin)
    if args.verify_swift_runtime:
        verify_swift_runtime(args.swift, args.swift_library_dir)


if __name__ == "__main__":
    main()
