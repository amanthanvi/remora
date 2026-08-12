#!/usr/bin/env python3
"""Harden generated UniFFI sensitive transfers and fail on template drift."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile


def replace_once(source: str, old: str, new: str, label: str) -> str:
    """Apply one hardening transform or accept its exact prior application."""
    old_count = source.count(old)
    new_count = source.count(new)
    if new_count == 1:
        # Some hardened replacements intentionally retain the original text as
        # a prefix.  Accept only the occurrences contained by the one complete
        # hardened replacement; an additional raw occurrence is still drift.
        expected_old_count = new.count(old)
        if old_count != expected_old_count:
            raise SystemExit(
                f"error: generated binding drift for {label}: found the "
                f"hardened replacement plus {old_count - expected_old_count} "
                "unexpected raw matches"
            )
        return source
    if new_count != 0:
        raise SystemExit(
            f"error: generated binding drift for {label}: "
            f"expected at most 1 hardened match, found {new_count}"
        )
    if old_count != 1:
        raise SystemExit(
            f"error: generated binding drift for {label}: "
            f"expected 1 raw match, found {old_count}"
        )
    return source.replace(old, new, 1)


def replace_exact(
    source: str,
    old: str,
    new: str,
    expected: int,
    label: str,
) -> str:
    """Apply an exact number of identical transforms, preserving idempotence."""
    old_count = source.count(old)
    new_count = source.count(new)
    if new_count == expected:
        expected_old_count = expected * new.count(old)
        if old_count != expected_old_count:
            raise SystemExit(
                f"error: generated binding drift for {label}: found the "
                f"hardened replacements plus {old_count - expected_old_count} "
                "unexpected raw matches"
            )
        return source
    if new_count != 0:
        raise SystemExit(
            f"error: generated binding drift for {label}: expected either 0 or "
            f"{expected} hardened matches, found {new_count}"
        )
    if old_count != expected:
        raise SystemExit(
            f"error: generated binding drift for {label}: expected {expected} "
            f"raw matches, found {old_count}"
        )
    return source.replace(old, new)


def replace_once_upgrade(
    source: str,
    raw: str,
    legacy: str,
    hardened: str,
    label: str,
) -> str:
    """Upgrade one raw/legacy template or accept its exact hardened form."""
    counts = {
        "raw": source.count(raw),
        "legacy": source.count(legacy),
        "hardened": source.count(hardened),
    }
    present = [state for state, count in counts.items() if count != 0]
    if present == ["hardened"] and counts["hardened"] == 1:
        return source
    if present == ["legacy"] and counts["legacy"] == 1:
        return source.replace(legacy, hardened, 1)
    if present == ["raw"] and counts["raw"] == 1:
        return source.replace(raw, hardened, 1)
    raise SystemExit(
        f"error: generated binding drift for {label}: expected exactly one "
        "raw, legacy, or hardened match, found "
        f"raw={counts['raw']}, legacy={counts['legacy']}, "
        f"hardened={counts['hardened']}"
    )


def require_exact(source: str, needle: str, expected: int, label: str) -> None:
    count = source.count(needle)
    if count != expected:
        raise SystemExit(
            f"error: generated binding drift for {label}: "
            f"expected {expected} matches, found {count}"
        )


def verify_swift_device_database_key_hardening(source: str) -> None:
    """Fail closed when the optional database bridge stops using the secret carrier."""
    marker = "public protocol DeviceDatabaseBridgeProtocol: AnyObject, Sendable"
    if marker not in source:
        return
    require_exact(
        source,
        "public static func `open`(path: String, masterKey: AppRelaySecretValue)",
        1,
        "Swift device database master-key carrier",
    )
    require_exact(
        source,
        "FfiConverterTypeAppRelaySecretValue_lower(masterKey)",
        1,
        "Swift device database zeroizing master-key lower",
    )


def verify_kotlin_device_database_key_hardening(source: str) -> None:
    """Fail closed when the optional database bridge stops using the secret carrier."""
    marker = "public interface DeviceDatabaseBridgeInterface"
    if marker not in source:
        return
    require_exact(
        source,
        "fun `open`(`path`: kotlin.String, `masterKey`: AppRelaySecretValue)",
        1,
        "Kotlin device database master-key carrier",
    )
    require_exact(
        source,
        "FfiConverterTypeAppRelaySecretValue.lower(`masterKey`)",
        1,
        "Kotlin device database zeroizing master-key lower",
    )


SWIFT_UNIFFI_ASYNC_HELPER = """fileprivate func uniffiRustCallAsync<F, T>(
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


SWIFT_CANCELLABLE_ASYNC_SUPPORT = """fileprivate final class UniffiRustFutureCancellationCoordinator: @unchecked Sendable {
    private let lock = NSLock()
    private let cancelFunc: (UInt64) -> ()
    private let freeFunc: (UInt64) -> ()
    private var rustFuture: UInt64?
    private var cancellationRequested = false
    private var cancelInvoked = false
    private var completionStarted = false
    private var freed = false

    init(
        cancelFunc: @escaping (UInt64) -> (),
        freeFunc: @escaping (UInt64) -> ()
    ) {
        self.cancelFunc = cancelFunc
        self.freeFunc = freeFunc
    }

    func register(rustFuture: UInt64) {
        lock.lock()
        defer { lock.unlock() }
        precondition(self.rustFuture == nil, "Rust future registered twice")
        precondition(!freed, "Rust future registered after free")
        self.rustFuture = rustFuture
        cancelIfNeededLocked()
    }

    func poll(_ body: (UInt64) -> ()) {
        lock.lock()
        defer { lock.unlock() }
        guard let rustFuture, !completionStarted, !freed else {
            fatalError("invalid Rust future poll state")
        }
        body(rustFuture)
    }

    func complete<F>(_ body: (UInt64) -> F) -> F {
        lock.lock()
        defer { lock.unlock() }
        guard let rustFuture, !completionStarted, !freed else {
            fatalError("invalid Rust future completion state")
        }
        completionStarted = true
        return body(rustFuture)
    }

    func cancel() {
        lock.lock()
        defer { lock.unlock() }
        cancellationRequested = true
        cancelIfNeededLocked()
    }

    func free() {
        lock.lock()
        defer { lock.unlock() }
        guard let rustFuture, !freed else { return }
        freed = true
        freeFunc(rustFuture)
    }

    private func cancelIfNeededLocked() {
        guard cancellationRequested,
              let rustFuture,
              !cancelInvoked,
              !completionStarted,
              !freed else { return }
        cancelInvoked = true
        cancelFunc(rustFuture)
    }
}

fileprivate func uniffiRustCallAsyncCancellable<F, T>(
    rustFutureFunc: () -> UInt64,
    pollFunc: (UInt64, @escaping UniffiRustFutureContinuationCallback, UInt64) -> (),
    completeFunc: (UInt64, UnsafeMutablePointer<RustCallStatus>) -> F,
    cancelFunc: @escaping (UInt64) -> (),
    freeFunc: @escaping (UInt64) -> (),
    liftFunc: (F) throws -> T,
    errorHandler: ((RustBuffer) throws -> Swift.Error)?
) async throws -> T {
    let coordinator = UniffiRustFutureCancellationCoordinator(
        cancelFunc: cancelFunc,
        freeFunc: freeFunc
    )
    return try await withTaskCancellationHandler(operation: {
        // Future creation has no RustCallStatus and therefore does not call
        // makeRustCall(), so initialization is still required here.
        uniffiEnsureCodexMobileClientInitialized()
        coordinator.register(rustFuture: rustFutureFunc())
        defer { coordinator.free() }

        var pollResult: Int8
        repeat {
            pollResult = await withUnsafeContinuation { continuation in
                let handle = uniffiContinuationHandleMap.insert(obj: continuation)
                coordinator.poll { rustFuture in
                    pollFunc(
                        rustFuture,
                        { handle, pollResult in
                            uniffiFutureContinuationCallback(
                                handle: handle,
                                pollResult: pollResult
                            )
                        },
                        handle
                    )
                }
            }
        } while pollResult != UNIFFI_RUST_FUTURE_POLL_READY

        var callStatus = RustCallStatus.init()
        let returnValue = coordinator.complete { rustFuture in
            completeFunc(rustFuture, &callStatus)
        }
        if callStatus.code == CALL_CANCELLED {
            if callStatus.errorBuf.data != nil {
                callStatus.errorBuf.deallocate()
            }
            throw CancellationError()
        }
        try uniffiCheckCallStatus(
            callStatus: callStatus,
            errorHandler: errorHandler
        )
        return try liftFunc(returnValue)
    }, onCancel: {
        coordinator.cancel()
    })
}
"""


def swift_rust_buffer_app_client_method(
    method: str,
    signature: str,
    rust_function: str,
    lowered_arguments: str,
    outcome: str,
    async_helper: str,
    include_cancel: bool,
) -> str:
    arguments = f"                    {lowered_arguments}\n" if lowered_arguments else ""
    cancel_argument = (
        "            cancelFunc: "
        "ffi_codex_mobile_client_rust_future_cancel_rust_buffer,\n"
        if include_cancel
        else ""
    )
    return f"""open func {method}({signature})async throws  -> {outcome}  {{
    return
        try  await {async_helper}(
            rustFutureFunc: {{
                {rust_function}(
                    self.uniffiCloneHandle(),
{arguments}                )
            }},
            pollFunc: ffi_codex_mobile_client_rust_future_poll_rust_buffer,
            completeFunc: ffi_codex_mobile_client_rust_future_complete_rust_buffer,
{cancel_argument}            freeFunc: ffi_codex_mobile_client_rust_future_free_rust_buffer,
            liftFunc: FfiConverterType{outcome}_lift,
            errorHandler: FfiConverterTypeRemoraLinkError_lift
        )
}}
"""


def swift_callback_method_span(
    source: str,
    backend_name: str,
    method: str,
) -> tuple[int, int]:
    backend_header = f"fileprivate struct UniffiCallbackInterface{backend_name} {{"
    require_exact(
        source,
        backend_header,
        1,
        f"Swift {backend_name} callback implementation",
    )
    backend_start = source.index(backend_header)
    backend_end_marker = f"\nprivate func uniffiCallbackInit{backend_name}()"
    require_exact(
        source,
        backend_end_marker,
        1,
        f"Swift {backend_name} callback boundary",
    )
    backend_end = source.index(backend_end_marker, backend_start)
    backend_source = source[backend_start:backend_end]
    method_marker = f"        {method}: {{ ("
    require_exact(
        backend_source,
        method_marker,
        1,
        f"Swift {backend_name}.{method} callback method",
    )
    method_start = backend_start + backend_source.index(method_marker)
    next_method = re.search(
        r"^        [A-Za-z_][A-Za-z0-9_]*: \{ \($",
        source[method_start + len(method_marker) : backend_end],
        flags=re.MULTILINE,
    )
    vtable_end = source.find("\n    )]\n}", method_start, backend_end)
    if vtable_end < 0:
        raise SystemExit(
            f"error: generated binding drift for Swift {backend_name}.{method}: "
            "missing callback-vtable boundary"
        )
    if next_method is None:
        method_end = vtable_end
    else:
        method_end = method_start + len(method_marker) + next_method.start()
    return method_start, method_end


def swift_callback_method(source: str, backend_name: str, method: str) -> str:
    start, end = swift_callback_method_span(source, backend_name, method)
    return source[start:end]


def replace_once_in_swift_callback(
    source: str,
    backend_name: str,
    method: str,
    old: str,
    new: str,
    label: str,
) -> str:
    start, end = swift_callback_method_span(source, backend_name, method)
    method_source = replace_once(source[start:end], old, new, label)
    return source[:start] + method_source + source[end:]


def harden_swift_sensitive_data_converter(
    source: str,
    type_name: str,
    label: str,
) -> str:
    bounded_pairing_code = type_name == "AppRemoraLinkPairingCode"
    raw_read = """    public static func read(from buf: inout (data: Data, offset: Data.Index)) throws -> AppRelaySecretValue {
        return try FfiConverterData.read(from: &buf)
    }
""".replace("AppRelaySecretValue", type_name)
    hardened_read = """    public static func read(from buf: inout (data: Data, offset: Data.Index)) throws -> AppRelaySecretValue {
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
""".replace("AppRelaySecretValue", type_name)
    if bounded_pairing_code:
        hardened_read = hardened_read.replace(
            """        let range = buf.offset..<(buf.offset + count)
        let secret = buf.data.withUnsafeBytes { bytes in
            AppRemoraLinkPairingCode(
""",
            """        guard count <= AppRemoraLinkPairingCode.maximumByteCount else {
            throw UniffiInternalError.bufferOverflow
        }
        let range = buf.offset..<(buf.offset + count)
        let secret = try buf.data.withUnsafeBytes { bytes in
            try AppRemoraLinkPairingCode(
""",
            1,
        )
    source = replace_once(
        source,
        raw_read,
        hardened_read,
        f"Swift {label} converter nested read",
    )

    raw_write = """    public static func write(_ value: AppRelaySecretValue, into buf: inout [UInt8]) {
        return FfiConverterData.write(value, into: &buf)
    }
""".replace("AppRelaySecretValue", type_name)
    hardened_write = """    public static func write(_ value: AppRelaySecretValue, into buf: inout [UInt8]) {
        writeInt(&buf, Int32(value.count))
        value.withUnsafeBytes { writeBytes(&buf, $0) }
    }
""".replace("AppRelaySecretValue", type_name)
    source = replace_once(
        source,
        raw_write,
        hardened_write,
        f"Swift {label} converter nested write",
    )

    raw_lift = """    public static func lift(_ value: RustBuffer) throws -> AppRelaySecretValue {
        return try FfiConverterData.lift(value)
    }
""".replace("AppRelaySecretValue", type_name)
    hardened_lift = """    public static func lift(_ value: RustBuffer) throws -> AppRelaySecretValue {
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
""".replace("AppRelaySecretValue", type_name)
    if bounded_pairing_code:
        hardened_lift = hardened_lift.replace(
            """        guard secretCount <= Int(Int32.max) else {
            throw UniffiInternalError.bufferOverflow
        }
""",
            """        guard secretCount <= AppRemoraLinkPairingCode.maximumByteCount else {
            throw UniffiInternalError.bufferOverflow
        }
""",
            1,
        ).replace(
            """        return AppRemoraLinkPairingCode(
""",
            """        return try AppRemoraLinkPairingCode(
""",
            1,
        )
    source = replace_once(
        source,
        raw_lift,
        hardened_lift,
        f"Swift {label} converter direct lift",
    )

    raw_lower = """    public static func lower(_ value: AppRelaySecretValue) -> RustBuffer {
        return FfiConverterData.lower(value)
    }
""".replace("AppRelaySecretValue", type_name)
    hardened_lower = """    public static func lower(_ value: AppRelaySecretValue) -> RustBuffer {
        return value.lowerToRustBufferAndZeroize()
    }
""".replace("AppRelaySecretValue", type_name)
    return replace_once(
        source,
        raw_lower,
        hardened_lower,
        f"Swift {label} converter direct lowering",
    )


def harden_swift_source(source: str) -> str:
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
    cancellable_support = (
        SWIFT_UNIFFI_ASYNC_HELPER + "\n" + SWIFT_CANCELLABLE_ASYNC_SUPPORT
    )
    if (
        "uniffiRustCallAsyncCancellable(" in source
        or "fileprivate func uniffiRustCallAsyncCancellable<" in source
    ):
        require_exact(
            source,
            cancellable_support,
            1,
            "Swift cancellable UniFFI async support",
        )
    else:
        source = replace_once(
            source,
            SWIFT_UNIFFI_ASYNC_HELPER,
            cancellable_support,
            "Swift UniFFI async helper template",
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
    relay_carrier_start = source.index(
        "/// A single-allocation, reference-semantic relay-secret carrier."
    )
    private_converter_declaration = """
#if swift(>=5.8)
@_documentation(visibility: private)
#endif
public struct FfiConverterTypeAppRelaySecretValue: FfiConverter"""
    relay_carrier_end = source.index(
        private_converter_declaration,
        relay_carrier_start,
    )
    pairing_carrier = source[relay_carrier_start:relay_carrier_end]
    pairing_carrier = pairing_carrier.replace(
        "AppRelaySecretValue", "AppRemoraLinkPairingCode"
    ).replace("relay-secret", "pairing-code").replace(
        "relay secret", "pairing code"
    )
    pairing_carrier = pairing_carrier.replace(
        """public final class AppRemoraLinkPairingCode: @unchecked Sendable {
    private static let lengthPrefixCount = MemoryLayout<Int32>.size
""",
        """public final class AppRemoraLinkPairingCode: @unchecked Sendable {
    public static let maximumByteCount = 4_128
    private static let lengthPrefixCount = MemoryLayout<Int32>.size
""",
        1,
    )
    for input_name, input_type in (
        ("bytes", "UnsafeRawBufferPointer"),
        ("data", "Data"),
        ("bytes", "[UInt8]"),
    ):
        pairing_carrier = pairing_carrier.replace(
            f"""    public init(copying {input_name}: {input_type}) {{
        precondition({input_name}.count <= Int(Int32.max), "pairing code is too large")
""",
            f"""    public init(copying {input_name}: {input_type}) throws {{
        guard {input_name}.count <= Self.maximumByteCount else {{
            throw RemoraLinkError.InvalidPairingCode
        }}
""",
            1,
        )
    require_exact(
        pairing_carrier,
        "throw RemoraLinkError.InvalidPairingCode",
        3,
        "Swift checked pairing-code construction bound",
    )
    source = replace_once(
        source,
        "public typealias AppRemoraLinkPairingCode = Data\n",
        pairing_carrier + "\n",
        "Swift reference pairing-code carrier",
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
    source = harden_swift_sensitive_data_converter(
        source,
        "AppRemoraLinkPairingCode",
        "pairing-code",
    )
    source = replace_once(
        source,
        """fileprivate struct FfiConverterOptionTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer {
    typealias SwiftType = AppRemoraLinkPairingCode?

    public static func write(_ value: SwiftType, into buf: inout [UInt8]) {
""",
        """fileprivate struct FfiConverterOptionTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer {
    typealias SwiftType = AppRemoraLinkPairingCode?

    public static func lower(_ value: SwiftType) -> RustBuffer {
        var writer = createWriter()
        defer {
            writer.withUnsafeMutableBytes { bytes in
                guard let baseAddress = bytes.baseAddress else { return }
                zeroizeRelaySecretMemory(baseAddress, byteCount: bytes.count)
            }
            value?.zeroize()
        }
        write(value, into: &writer)
        return RustBuffer(bytes: writer)
    }

    public static func write(_ value: SwiftType, into buf: inout [UInt8]) {
""",
        "Swift optional pairing-code converter",
    )
    for method, signature, rust_function, lowered_arguments, outcome in (
        (
            "inspectRemoraLinkCode",
            "code: AppRemoraLinkPairingCode",
            "uniffi_codex_mobile_client_fn_method_appclient_inspect_remora_link_code",
            "FfiConverterTypeAppRemoraLinkPairingCode_lower(code)",
            "AppRemoraLinkInspection",
        ),
        (
            "acceptRemoraLinkOffer",
            "acceptance: AppRemoraLinkAcceptance",
            "uniffi_codex_mobile_client_fn_method_appclient_accept_remora_link_offer",
            "FfiConverterTypeAppRemoraLinkAcceptance_lower(acceptance)",
            "AppRemoraLinkPairingOutcome",
        ),
        (
            "awaitRemoraLinkPairing",
            "hostId: String, code: AppRemoraLinkPairingCode?",
            "uniffi_codex_mobile_client_fn_method_appclient_await_remora_link_pairing",
            "FfiConverterString.lower(hostId),FfiConverterOptionTypeAppRemoraLinkPairingCode.lower(code)",
            "AppRemoraLinkPairingOutcome",
        ),
    ):
        raw_method = swift_rust_buffer_app_client_method(
            method,
            signature,
            rust_function,
            lowered_arguments,
            outcome,
            "uniffiRustCallAsync",
            False,
        )
        cancellable_method = swift_rust_buffer_app_client_method(
            method,
            signature,
            rust_function,
            lowered_arguments,
            outcome,
            "uniffiRustCallAsyncCancellable",
            True,
        )
        source = replace_once(
            source,
            raw_method,
            cancellable_method,
            f"Swift {method} cancellable future",
        )
    source = replace_once(
        source,
        """open func backgroundRelayStageEnrollment(hostId: String, relayOrigin: String, installationId: String, commandId: String, readCapability: AppRelaySecretValue, manageCapability: AppRelaySecretValue)async throws   {
    return
""",
        """open func backgroundRelayStageEnrollment(hostId: String, relayOrigin: String, installationId: String, commandId: String, readCapability: AppRelaySecretValue, manageCapability: AppRelaySecretValue)async throws   {
    let manageCapabilityForLowering: AppRelaySecretValue
    if manageCapability === readCapability {
        manageCapabilityForLowering = manageCapability.withUnsafeBytes {
            AppRelaySecretValue(copying: $0)
        }
    } else {
        manageCapabilityForLowering = manageCapability
    }
    defer {
        readCapability.zeroize()
        manageCapabilityForLowering.zeroize()
    }
    return
""",
        "Swift aliased enrollment secret preparation",
    )
    source = replace_once(
        source,
        "FfiConverterTypeAppRelaySecretValue_lower(readCapability),FfiConverterTypeAppRelaySecretValue_lower(manageCapability)",
        "FfiConverterTypeAppRelaySecretValue_lower(readCapability),FfiConverterTypeAppRelaySecretValue_lower(manageCapabilityForLowering)",
        "Swift aliased enrollment secret lowering",
    )
    secret_result_callback = """            let uniffiHandleSuccess = { (returnValue: AppRelaySecretValue) in
                uniffiFutureCallback(
                    uniffiCallbackData,
                    UniffiForeignFutureResultRustBuffer(
                        returnValue: FfiConverterTypeAppRelaySecretValue_lower(returnValue),
                        callStatus: RustCallStatus()
                    )
                )
            }
"""
    hardened_secret_result_callback = """            let uniffiHandleSuccess = { (returnValue: AppRelaySecretValue) in
                defer { returnValue.zeroize() }
                uniffiFutureCallback(
                    uniffiCallbackData,
                    UniffiForeignFutureResultRustBuffer(
                        returnValue: FfiConverterTypeAppRelaySecretValue_lower(returnValue),
                        callStatus: RustCallStatus()
                    )
                )
            }
"""
    for backend_name, method in (
        ("AppRelaySecretBackend", "read"),
        ("AppRemoraLinkDeviceKeyBackend", "signMessage"),
        ("AppRemoraLinkTransportIdentityBackend", "loadOrCreate"),
    ):
        source = replace_once_in_swift_callback(
            source,
            backend_name,
            method,
            secret_result_callback,
            hardened_secret_result_callback,
            f"Swift {backend_name}.{method} secret-result callback",
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
    source = replace_once(
        source,
        """            let makeCall = {
                () async throws -> AppRelaySecretValue in
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRemoraLinkDeviceKeyBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                return try await uniffiObj.signMessage(
                     slot: try FfiConverterString.lift(slot),
                     message: try FfiConverterTypeAppRelaySecretValue_lift(message)
                )
            }
""",
        """            let makeCall = {
                () async throws -> AppRelaySecretValue in
                let secretValue = try FfiConverterTypeAppRelaySecretValue_lift(message)
                defer { secretValue.zeroize() }
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRemoraLinkDeviceKeyBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                let returnValue = try await uniffiObj.signMessage(
                     slot: try FfiConverterString.lift(slot),
                     message: secretValue
                )
                if returnValue === secretValue {
                    return returnValue.withUnsafeBytes {
                        AppRelaySecretValue(copying: $0)
                    }
                }
                return returnValue
            }
""",
        "Swift device-signing message callback",
    )
    source = replace_once(
        source,
        """            let makeCall = {
                () async throws -> AppRelaySecretValue in
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRemoraLinkTransportIdentityBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                return try await uniffiObj.loadOrCreate(
                     candidate: try FfiConverterTypeAppRelaySecretValue_lift(candidate)
                )
            }
""",
        """            let makeCall = {
                () async throws -> AppRelaySecretValue in
                let secretValue = try FfiConverterTypeAppRelaySecretValue_lift(candidate)
                defer { secretValue.zeroize() }
                guard let uniffiObj = try? FfiConverterCallbackInterfaceAppRemoraLinkTransportIdentityBackend.handleMap.get(handle: uniffiHandle) else {
                    throw UniffiInternalError.unexpectedStaleHandle
                }
                let returnValue = try await uniffiObj.loadOrCreate(
                     candidate: secretValue
                )
                if returnValue === secretValue {
                    return returnValue.withUnsafeBytes {
                        AppRelaySecretValue(copying: $0)
                    }
                }
                return returnValue
            }
""",
        "Swift transport-identity candidate callback",
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
        "message: AppRelaySecretValue",
        1,
        "Swift device-signing message argument",
    )
    require_exact(
        source,
        "candidate: AppRelaySecretValue",
        1,
        "Swift transport-identity candidate argument",
    )
    require_exact(
        source,
        "public final class AppRemoraLinkPairingCode: @unchecked Sendable",
        1,
        "Swift reference pairing-code carrier",
    )
    require_exact(
        source,
        "public static let maximumByteCount = 4_128",
        1,
        "Swift pairing-code construction bound",
    )
    require_exact(
        source,
        "throw RemoraLinkError.InvalidPairingCode",
        3,
        "Swift checked oversized pairing-code rejection",
    )
    require_exact(
        source,
        'precondition(bytes.count <= Int(Int32.max), "pairing code is too large")',
        0,
        "Swift absence of trapping pairing-code bound",
    )
    pairing_converter_documentation = """#if swift(>=5.8)
@_documentation(visibility: private)
#endif
public struct FfiConverterTypeAppRemoraLinkPairingCode: FfiConverter"""
    require_exact(
        source,
        pairing_converter_documentation,
        1,
        "Swift pairing-code converter documentation",
    )
    require_exact(
        source,
        pairing_converter_documentation.replace(
            "public struct",
            "#if swift(>=5.8)\n"
            "@_documentation(visibility: private)\n"
            "#endif\n"
            "public struct",
        ),
        0,
        "Swift duplicate pairing-code converter documentation",
    )
    require_exact(
        source,
        "public typealias AppRemoraLinkPairingCode = Data",
        0,
        "Swift absence of immutable pairing-code alias",
    )
    require_exact(
        source,
        "FfiConverterTypeAppRemoraLinkPairingCode_lower(code)",
        1,
        "Swift direct pairing-code lowering call",
    )
    require_exact(
        source,
        "FfiConverterOptionTypeAppRemoraLinkPairingCode.lower(code)",
        1,
        "Swift optional pairing-code lowering call",
    )
    optional_pairing_converter_start = source.index(
        "fileprivate struct FfiConverterOptionTypeAppRemoraLinkPairingCode: "
        "FfiConverterRustBuffer {"
    )
    optional_pairing_converter_end = source.index(
        "\n}", optional_pairing_converter_start
    ) + len("\n}")
    optional_pairing_converter = source[
        optional_pairing_converter_start:optional_pairing_converter_end
    ]
    require_exact(
        optional_pairing_converter,
        "defer {\n            writer.withUnsafeMutableBytes",
        1,
        "Swift optional pairing-code transfer cleanup",
    )
    require_exact(
        optional_pairing_converter,
        "value?.zeroize()",
        1,
        "Swift optional pairing-code carrier cleanup",
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
        2,
        "Swift direct sensitive-value zeroizing lowers",
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
        SWIFT_UNIFFI_ASYNC_HELPER,
        1,
        "Swift unchanged ordinary UniFFI async helper",
    )
    require_exact(
        source,
        SWIFT_CANCELLABLE_ASYNC_SUPPORT,
        1,
        "Swift cancellable UniFFI async support",
    )
    require_exact(
        source,
        "uniffiRustCallAsyncCancellable(",
        3,
        "Swift Remora Link cancellable method calls",
    )
    require_exact(
        source,
        "cancelFunc: ffi_codex_mobile_client_rust_future_cancel_rust_buffer",
        3,
        "Swift Remora Link RustBuffer cancellation hooks",
    )
    require_exact(
        source,
        "defer { secretValue.zeroize() }",
        5,
        "Swift callback secret completion wipe",
    )
    for backend_name, method in (
        ("AppRelaySecretBackend", "read"),
        ("AppRemoraLinkDeviceKeyBackend", "signMessage"),
        ("AppRemoraLinkTransportIdentityBackend", "loadOrCreate"),
    ):
        require_exact(
            swift_callback_method(source, backend_name, method),
            "defer { returnValue.zeroize() }",
            1,
            f"Swift {backend_name}.{method} result completion wipe",
        )
    verify_swift_device_database_key_hardening(source)
    return source


def harden_swift(path: Path) -> None:
    source = harden_swift_source(path.read_text())
    path.write_text(source)


SWIFT_CANCELLATION_RUNTIME_VERIFIER = """

final class UniffiCancellationProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var callback: UniffiRustFutureContinuationCallback?
    private var callbackData: UInt64 = 0
    private var cancelled = false
    private var cancelCount = 0
    private var freeCount = 0
    private let readyImmediately: Bool
    private let completeEntered: DispatchSemaphore?
    private let waitForTaskCancellation: Bool
    let pollRegistered = DispatchSemaphore(value: 0)

    init(
        readyImmediately: Bool = false,
        completeEntered: DispatchSemaphore? = nil,
        waitForTaskCancellation: Bool = false
    ) {
        self.readyImmediately = readyImmediately
        self.completeEntered = completeEntered
        self.waitForTaskCancellation = waitForTaskCancellation
    }

    func poll(
        _ handle: UInt64,
        _ callback: @escaping UniffiRustFutureContinuationCallback,
        _ callbackData: UInt64
    ) {
        precondition(handle != 0)
        lock.lock()
        let shouldResume = cancelled || readyImmediately
        if !shouldResume {
            self.callback = callback
            self.callbackData = callbackData
        }
        lock.unlock()
        pollRegistered.signal()
        if shouldResume {
            callback(callbackData, UNIFFI_RUST_FUTURE_POLL_READY)
        }
    }

    func cancel(_ handle: UInt64) {
        precondition(handle != 0)
        lock.lock()
        cancelCount += 1
        cancelled = true
        let callback = self.callback
        let callbackData = self.callbackData
        self.callback = nil
        lock.unlock()
        callback?(callbackData, UNIFFI_RUST_FUTURE_POLL_READY)
    }

    func complete(
        _ handle: UInt64,
        _ callStatus: UnsafeMutablePointer<RustCallStatus>
    ) -> UInt8 {
        precondition(handle != 0)
        completeEntered?.signal()
        if waitForTaskCancellation {
            let deadline = Date().addingTimeInterval(5)
            while !Task.isCancelled && Date() < deadline {
                Thread.sleep(forTimeInterval: 0.0001)
            }
            precondition(Task.isCancelled, "completion never overlapped cancellation")
        }
        lock.lock()
        let wasCancelled = cancelled
        lock.unlock()
        if wasCancelled {
            callStatus.pointee.code = CALL_CANCELLED
        }
        return 42
    }

    func free(_ handle: UInt64) {
        precondition(handle != 0)
        lock.lock()
        freeCount += 1
        lock.unlock()
    }

    func counts() -> (cancel: Int, free: Int) {
        lock.lock()
        defer { lock.unlock() }
        return (cancelCount, freeCount)
    }
}

fileprivate func runCancellableFuture(
    probe: UniffiCancellationProbe,
    rustFutureFunc: @escaping () -> UInt64 = { 1 }
) async throws -> UInt8 {
    try await uniffiRustCallAsyncCancellable(
        rustFutureFunc: rustFutureFunc,
        pollFunc: { probe.poll($0, $1, $2) },
        completeFunc: { probe.complete($0, $1) },
        cancelFunc: { probe.cancel($0) },
        freeFunc: { probe.free($0) },
        liftFunc: { $0 },
        errorHandler: nil
    )
}

fileprivate func expectCancellation(
    _ task: Task<UInt8, Swift.Error>,
    scenario: String
) async {
    do {
        _ = try await task.value
        fatalError("expected CancellationError for \\(scenario)")
    } catch is CancellationError {
        // Expected.
    } catch {
        fatalError("unexpected \\(scenario) error: \\(error)")
    }
}

let beforeRegistrationProbe = UniffiCancellationProbe()
let futureCreationEntered = DispatchSemaphore(value: 0)
let allowFutureRegistration = DispatchSemaphore(value: 0)
let beforeRegistrationTask = Task.detached {
    try await runCancellableFuture(
        probe: beforeRegistrationProbe,
        rustFutureFunc: {
            futureCreationEntered.signal()
            precondition(
                allowFutureRegistration.wait(timeout: .now() + 5) == .success,
                "timed out waiting to register fake Rust future"
            )
            return 1
        }
    )
}
precondition(futureCreationEntered.wait(timeout: .now() + 5) == .success)
beforeRegistrationTask.cancel()
allowFutureRegistration.signal()
await expectCancellation(
    beforeRegistrationTask,
    scenario: "cancel before registration"
)
precondition(beforeRegistrationProbe.counts().cancel == 1)
precondition(beforeRegistrationProbe.counts().free == 1)

let duringPollProbe = UniffiCancellationProbe()
let duringPollTask = Task.detached {
    try await runCancellableFuture(probe: duringPollProbe)
}
precondition(duringPollProbe.pollRegistered.wait(timeout: .now() + 5) == .success)
duringPollTask.cancel()
await expectCancellation(duringPollTask, scenario: "cancel during poll")
precondition(duringPollProbe.counts().cancel == 1)
precondition(duringPollProbe.counts().free == 1)

let completionEntered = DispatchSemaphore(value: 0)
let completionWonProbe = UniffiCancellationProbe(
    readyImmediately: true,
    completeEntered: completionEntered,
    waitForTaskCancellation: true
)
let completionWonTask = Task.detached {
    try await runCancellableFuture(probe: completionWonProbe)
}
precondition(completionEntered.wait(timeout: .now() + 5) == .success)
completionWonTask.cancel()
let completionValue = try await completionWonTask.value
precondition(completionValue == 42)
precondition(completionWonProbe.counts().cancel == 0)
precondition(completionWonProbe.counts().free == 1)

for iteration in 0..<250 {
    let probe = UniffiCancellationProbe()
    let coordinator = UniffiRustFutureCancellationCoordinator(
        cancelFunc: { probe.cancel($0) },
        freeFunc: { probe.free($0) }
    )
    coordinator.register(rustFuture: UInt64(iteration + 1))
    let start = DispatchSemaphore(value: 0)
    let group = DispatchGroup()
    group.enter()
    DispatchQueue.global().async {
        start.wait()
        _ = coordinator.complete { _ in UInt8(42) }
        group.leave()
    }
    group.enter()
    DispatchQueue.global().async {
        start.wait()
        coordinator.cancel()
        group.leave()
    }
    start.signal()
    start.signal()
    precondition(group.wait(timeout: .now() + 5) == .success)
    coordinator.cancel()
    coordinator.free()
    coordinator.free()
    let counts = probe.counts()
    precondition(counts.cancel == 0 || counts.cancel == 1)
    precondition(counts.free == 1)
}

for iteration in 0..<250 {
    let probe = UniffiCancellationProbe()
    let coordinator = UniffiRustFutureCancellationCoordinator(
        cancelFunc: { probe.cancel($0) },
        freeFunc: { probe.free($0) }
    )
    coordinator.register(rustFuture: UInt64(iteration + 1))
    let start = DispatchSemaphore(value: 0)
    let group = DispatchGroup()
    group.enter()
    DispatchQueue.global().async {
        start.wait()
        coordinator.cancel()
        group.leave()
    }
    group.enter()
    DispatchQueue.global().async {
        start.wait()
        coordinator.free()
        group.leave()
    }
    start.signal()
    start.signal()
    precondition(group.wait(timeout: .now() + 5) == .success)
    coordinator.cancel()
    coordinator.free()
    let counts = probe.counts()
    precondition(counts.cancel == 0 || counts.cancel == 1)
    precondition(counts.free == 1)
}
"""


def verify_swift_runtime(path: Path, library_dir: Path) -> None:
    verifier = SWIFT_CANCELLATION_RUNTIME_VERIFIER + """

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

let pairingCode = try AppRemoraLinkPairingCode(copying: expected)
let retainedPairingAlias = pairingCode
let loweredPairingCode = FfiConverterTypeAppRemoraLinkPairingCode_lower(pairingCode)
precondition(retainedPairingAlias.withUnsafeBytes { $0.allSatisfy { $0 == 0 } })
let liftedPairingCode = try FfiConverterTypeAppRemoraLinkPairingCode_lift(loweredPairingCode)
precondition(liftedPairingCode.withUnsafeBytes { bytes in
    guard bytes.count == expected.count else { return false }
    for index in expected.indices where bytes[index] != expected[index] {
        return false
    }
    return true
})
liftedPairingCode.zeroize()

do {
    _ = try AppRemoraLinkPairingCode(
        copying: [UInt8](
            repeating: 0x41,
            count: AppRemoraLinkPairingCode.maximumByteCount + 1
        )
    )
    fatalError("oversized pairing code was accepted")
} catch RemoraLinkError.InvalidPairingCode {
    // Expected: rejection occurs before a carrier or RustBuffer is allocated.
} catch {
    fatalError("unexpected oversized pairing-code error: \\(error)")
}

print("Swift cancellation and sensitive-carrier runtime verification passed")
"""
    with tempfile.TemporaryDirectory(prefix="remora-secret-swift-") as temp_dir:
        temp = Path(temp_dir)
        main_path = temp / "main.swift"
        executable = temp / "relay-secret-runtime-test"
        # Append the verifier to the generated source so it exercises the
        # fileprivate cancellation helper rather than a test-only copy.
        main_path.write_text(path.read_text() + verifier)
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


KOTLIN_SECRET_BACKEND_HEADER = (
    "internal object uniffiCallbackInterfaceAppRelaySecretBackend {"
)
KOTLIN_SECRET_CALLBACK_METHODS = (
    "read",
    "write",
    "createIfAbsent",
    "revision",
    "compareAndSwap",
    "compareAndTombstone",
    "delete",
)
KOTLIN_SECRET_VALUE_METHODS = ("write", "createIfAbsent", "compareAndSwap")


def kotlin_callback_method_span(
    source: str,
    backend_name: str,
    method: str,
) -> tuple[int, int]:
    backend_header = f"internal object uniffiCallbackInterface{backend_name} {{"
    require_exact(
        source,
        backend_header,
        1,
        f"Kotlin {backend_name} callback object",
    )
    backend_start = source.index(backend_header)
    backend_methods_end = source.find(
        "\n    internal object uniffiFree:", backend_start
    )
    if backend_methods_end < 0:
        raise SystemExit(
            f"error: generated binding drift for Kotlin {backend_name}: missing "
            "uniffiFree boundary"
        )
    backend_methods = source[backend_start:backend_methods_end]
    method_marker = f"    internal object `{method}`:"
    require_exact(
        backend_methods,
        method_marker,
        1,
        f"Kotlin {backend_name}.{method} callback method",
    )
    method_start = backend_start + backend_methods.index(method_marker)
    next_method = source.find(
        "\n    internal object `", method_start + len(method_marker)
    )
    if next_method < 0 or next_method > backend_methods_end:
        method_end = backend_methods_end
    else:
        method_end = next_method
    return method_start, method_end


def kotlin_secret_backend_method_span(source: str, method: str) -> tuple[int, int]:
    return kotlin_callback_method_span(source, "AppRelaySecretBackend", method)


def kotlin_secret_backend_method(source: str, method: str) -> str:
    start, end = kotlin_secret_backend_method_span(source, method)
    return source[start:end]


def kotlin_callback_method(source: str, backend_name: str, method: str) -> str:
    start, end = kotlin_callback_method_span(source, backend_name, method)
    return source[start:end]


def replace_once_in_kotlin_secret_callback(
    source: str,
    method: str,
    old: str,
    new: str,
    label: str,
) -> str:
    start, end = kotlin_secret_backend_method_span(source, method)
    method_source = replace_once(source[start:end], old, new, label)
    return source[:start] + method_source + source[end:]


def replace_once_in_kotlin_callback(
    source: str,
    backend_name: str,
    method: str,
    old: str,
    new: str,
    label: str,
) -> str:
    start, end = kotlin_callback_method_span(source, backend_name, method)
    method_source = replace_once(source[start:end], old, new, label)
    return source[:start] + method_source + source[end:]


def verify_kotlin_secret_callback_hardening(source: str) -> None:
    backend_start = source.index(KOTLIN_SECRET_BACKEND_HEADER)
    backend_methods_end = source.index(
        "\n    internal object uniffiFree:", backend_start
    )
    methods = tuple(
        re.findall(
            r"^    internal object `([^`]+)`:.*$",
            source[backend_start:backend_methods_end],
            flags=re.MULTILINE,
        )
    )
    if methods != KOTLIN_SECRET_CALLBACK_METHODS:
        raise SystemExit(
            "error: generated binding drift for Kotlin relay-secret callbacks: "
            f"expected {KOTLIN_SECRET_CALLBACK_METHODS}, found {methods}"
        )

    for method in KOTLIN_SECRET_VALUE_METHODS:
        method_source = kotlin_secret_backend_method(source, method)
        require_exact(
            method_source,
            "val secretValue = FfiConverterTypeAppRelaySecretValue.lift(`value`)",
            1,
            f"Kotlin {method} lifted secret",
        )
        require_exact(
            method_source,
            "secretValue.fill(0)",
            2,
            f"Kotlin {method} secret cleanup",
        )
        require_exact(
            method_source,
            "{ secretValue.fill(0) },",
            1,
            f"Kotlin {method} completion cleanup",
        )

    for method in ("read", "revision", "compareAndTombstone", "delete"):
        require_exact(
            kotlin_secret_backend_method(source, method),
            "secretValue",
            0,
            f"Kotlin {method} absence of nonexistent secret cleanup",
        )

    require_exact(
        kotlin_secret_backend_method(source, "read"),
        "returnValue.fill(0)",
        1,
        "Kotlin read-result completion wipe",
    )


def verify_kotlin_remora_link_callback_hardening(source: str) -> None:
    for backend_name, expected_methods in (
        (
            "AppRemoraLinkDeviceKeyBackend",
            (
                "ensureHardwareKey",
                "loadHardwareKey",
                "signMessage",
                "deleteHardwareKey",
            ),
        ),
        ("AppRemoraLinkTransportIdentityBackend", ("loadOrCreate",)),
    ):
        header = f"internal object uniffiCallbackInterface{backend_name} {{"
        backend_start = source.index(header)
        backend_methods_end = source.index(
            "\n    internal object uniffiFree:", backend_start
        )
        methods = tuple(
            re.findall(
                r"^    internal object `([^`]+)`:.*$",
                source[backend_start:backend_methods_end],
                flags=re.MULTILINE,
            )
        )
        if methods != expected_methods:
            raise SystemExit(
                f"error: generated binding drift for Kotlin {backend_name} "
                f"callbacks: expected {expected_methods}, found {methods}"
            )

    for backend_name, method, argument in (
        ("AppRemoraLinkDeviceKeyBackend", "signMessage", "message"),
        (
            "AppRemoraLinkTransportIdentityBackend",
            "loadOrCreate",
            "candidate",
        ),
    ):
        method_source = kotlin_callback_method(source, backend_name, method)
        require_exact(
            method_source,
            f"val secretValue = FfiConverterTypeAppRelaySecretValue.lift(`{argument}`)",
            1,
            f"Kotlin {backend_name}.{method} lifted secret",
        )
        require_exact(
            method_source,
            "secretValue.fill(0)",
            2,
            f"Kotlin {backend_name}.{method} secret cleanup",
        )
        require_exact(
            method_source,
            "{ secretValue.fill(0) },",
            1,
            f"Kotlin {backend_name}.{method} completion cleanup",
        )
        require_exact(
            method_source,
            "returnValue.fill(0)",
            1,
            f"Kotlin {backend_name}.{method} result cleanup",
        )

    for method in ("ensureHardwareKey", "loadHardwareKey", "deleteHardwareKey"):
        require_exact(
            kotlin_callback_method(
                source, "AppRemoraLinkDeviceKeyBackend", method
            ),
            "secretValue",
            0,
            f"Kotlin {method} absence of nonexistent secret cleanup",
        )

    for needle, label in (
        (
            "start = kotlinx.coroutines.CoroutineStart.LAZY",
            "Kotlin lazy callback dispatch",
        ),
        (
            "job.invokeOnCompletion { onCompletion?.invoke() }",
            "Kotlin callback completion registration",
        ),
        ("job.start()", "Kotlin callback start after registration"),
    ):
        require_exact(source, needle, 2, label)


def harden_kotlin_source(source: str) -> str:
    raw_secret_converter = """public typealias AppRelaySecretValue = kotlin.ByteArray
public typealias FfiConverterTypeAppRelaySecretValue = FfiConverterByteArray
"""
    legacy_secret_converter = """public typealias AppRelaySecretValue = kotlin.ByteArray
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
"""
    hardened_secret_converter = legacy_secret_converter.replace(
        """            if (byteBuf.hasRemaining()) {
                throw RuntimeException("junk remaining in relay-secret transfer buffer")
""",
        """            if (byteBuf.hasRemaining()) {
                secret.fill(0)
                throw RuntimeException("junk remaining in relay-secret transfer buffer")
""",
        1,
    )
    source = replace_once_upgrade(
        source,
        raw_secret_converter,
        legacy_secret_converter,
        hardened_secret_converter,
        "Kotlin secret converter",
    )
    raw_pairing_converter = raw_secret_converter.replace(
        "AppRelaySecretValue", "AppRemoraLinkPairingCode"
    )
    legacy_pairing_converter = legacy_secret_converter.replace(
        "AppRelaySecretValue", "AppRemoraLinkPairingCode"
    ).replace("relay-secret", "pairing-code")
    hardened_pairing_converter = legacy_pairing_converter.replace(
        """            if (byteBuf.hasRemaining()) {
                throw RuntimeException("junk remaining in pairing-code transfer buffer")
""",
        """            if (byteBuf.hasRemaining()) {
                secret.fill(0)
                throw RuntimeException("junk remaining in pairing-code transfer buffer")
        """,
        1,
    )
    bounded_pairing_converter = """public class AppRemoraLinkPairingCode private constructor(
    private val storage: kotlin.ByteArray,
) {
    public companion object {
        public const val MAXIMUM_BYTE_COUNT: kotlin.Int = 4_128

        @Throws(RemoraLinkException::class)
        public fun copying(bytes: kotlin.ByteArray): AppRemoraLinkPairingCode {
            if (bytes.size > MAXIMUM_BYTE_COUNT) {
                throw RemoraLinkException.InvalidPairingCode()
            }
            return AppRemoraLinkPairingCode(bytes.copyOf())
        }

        internal fun takingOwnership(bytes: kotlin.ByteArray): AppRemoraLinkPairingCode {
            if (bytes.size > MAXIMUM_BYTE_COUNT) {
                bytes.fill(0)
                throw RuntimeException("pairing-code transfer exceeds maximum size")
            }
            return AppRemoraLinkPairingCode(bytes)
        }
    }

    internal fun <T> withBytes(block: (kotlin.ByteArray) -> T): T =
        synchronized(this) { block(storage) }

    public fun zeroize() {
        synchronized(this) { storage.fill(0) }
    }
}
public object FfiConverterTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer<AppRemoraLinkPairingCode> {
    override fun lift(value: RustBuffer.ByValue): AppRemoraLinkPairingCode {
        try {
            val byteBuf = value.asByteBuffer()
                ?: throw RuntimeException("null pairing-code transfer buffer")
            val secret = read(byteBuf)
            if (byteBuf.hasRemaining()) {
                secret.zeroize()
                throw RuntimeException("junk remaining in pairing-code transfer buffer")
            }
            return secret
        } finally {
            value.data?.setMemory(0, value.len, 0.toByte())
            RustBuffer.free(value)
        }
    }

    override fun read(buf: ByteBuffer): AppRemoraLinkPairingCode {
        val length = buf.getInt()
        if (length < 0 || length > AppRemoraLinkPairingCode.MAXIMUM_BYTE_COUNT) {
            throw RuntimeException("pairing-code transfer exceeds maximum size")
        }
        val bytes = kotlin.ByteArray(length)
        buf.get(bytes)
        return AppRemoraLinkPairingCode.takingOwnership(bytes)
    }

    override fun lower(value: AppRemoraLinkPairingCode): RustBuffer.ByValue =
        try {
            lowerIntoRustBuffer(value)
        } finally {
            value.zeroize()
        }

    override fun allocationSize(value: AppRemoraLinkPairingCode): ULong =
        value.withBytes { FfiConverterByteArray.allocationSize(it) }

    override fun write(value: AppRemoraLinkPairingCode, buf: ByteBuffer) =
        value.withBytes { FfiConverterByteArray.write(it, buf) }
}
"""
    if source.count(bounded_pairing_converter) == 0:
        source = replace_once_upgrade(
            source,
            raw_pairing_converter,
            legacy_pairing_converter,
            hardened_pairing_converter,
            "Kotlin pairing-code converter",
        )
        source = replace_once(
            source,
            hardened_pairing_converter,
            bounded_pairing_converter,
            "Kotlin bounded pairing-code converter",
        )
    elif source.count(bounded_pairing_converter) != 1:
        raise SystemExit(
            "error: generated binding drift for Kotlin bounded pairing-code "
            f"converter: expected 1 hardened match, found "
            f"{source.count(bounded_pairing_converter)}"
        )

    raw_optional_pairing_converter = """public object FfiConverterOptionalTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer<AppRemoraLinkPairingCode?> {
    override fun read(buf: ByteBuffer): AppRemoraLinkPairingCode? {
"""
    legacy_optional_pairing_converter = """public object FfiConverterOptionalTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer<AppRemoraLinkPairingCode?> {
    override fun lower(value: AppRemoraLinkPairingCode?): RustBuffer.ByValue =
        try {
            lowerIntoRustBuffer(value)
        } finally {
            value?.fill(0)
        }

    override fun read(buf: ByteBuffer): AppRemoraLinkPairingCode? {
"""
    bounded_optional_pairing_converter = """public object FfiConverterOptionalTypeAppRemoraLinkPairingCode: FfiConverterRustBuffer<AppRemoraLinkPairingCode?> {
    override fun lower(value: AppRemoraLinkPairingCode?): RustBuffer.ByValue =
        try {
            lowerIntoRustBuffer(value)
        } finally {
            value?.zeroize()
        }

    override fun read(buf: ByteBuffer): AppRemoraLinkPairingCode? {
"""
    source = replace_once_upgrade(
        source,
        raw_optional_pairing_converter,
        legacy_optional_pairing_converter,
        bounded_optional_pairing_converter,
        "Kotlin optional pairing-code converter",
    )
    source = replace_once(
        source,
        """    override suspend fun `backgroundRelayStageEnrollment`(`hostId`: kotlin.String, `relayOrigin`: kotlin.String, `installationId`: kotlin.String, `commandId`: kotlin.String, `readCapability`: AppRelaySecretValue, `manageCapability`: AppRelaySecretValue) {
        return uniffiRustCallAsync(
        callWithHandle { uniffiHandle ->
            UniffiLib.uniffi_codex_mobile_client_fn_method_appclient_background_relay_stage_enrollment(
                uniffiHandle,
                FfiConverterString.lower(`hostId`),FfiConverterString.lower(`relayOrigin`),FfiConverterString.lower(`installationId`),FfiConverterString.lower(`commandId`),FfiConverterTypeAppRelaySecretValue.lower(`readCapability`),FfiConverterTypeAppRelaySecretValue.lower(`manageCapability`),
            )
        },
        { future, callback, continuation -> UniffiLib.ffi_codex_mobile_client_rust_future_poll_void(future, callback, continuation) },
        { future, continuation -> UniffiLib.ffi_codex_mobile_client_rust_future_complete_void(future, continuation) },
        { future -> UniffiLib.ffi_codex_mobile_client_rust_future_free_void(future) },
        // lift function
        { Unit },
        \n        // Error FFI converter
        BackgroundRelayException.ErrorHandler,
    )
    }
""",
        """    override suspend fun `backgroundRelayStageEnrollment`(`hostId`: kotlin.String, `relayOrigin`: kotlin.String, `installationId`: kotlin.String, `commandId`: kotlin.String, `readCapability`: AppRelaySecretValue, `manageCapability`: AppRelaySecretValue) {
        val manageCapabilityForLowering =
            if (`manageCapability` === `readCapability`) `manageCapability`.copyOf()
            else `manageCapability`
        try {
            return uniffiRustCallAsync(
            callWithHandle { uniffiHandle ->
                UniffiLib.uniffi_codex_mobile_client_fn_method_appclient_background_relay_stage_enrollment(
                    uniffiHandle,
                    FfiConverterString.lower(`hostId`),FfiConverterString.lower(`relayOrigin`),FfiConverterString.lower(`installationId`),FfiConverterString.lower(`commandId`),FfiConverterTypeAppRelaySecretValue.lower(`readCapability`),FfiConverterTypeAppRelaySecretValue.lower(manageCapabilityForLowering),
                )
            },
            { future, callback, continuation -> UniffiLib.ffi_codex_mobile_client_rust_future_poll_void(future, callback, continuation) },
            { future, continuation -> UniffiLib.ffi_codex_mobile_client_rust_future_complete_void(future, continuation) },
            { future -> UniffiLib.ffi_codex_mobile_client_rust_future_free_void(future) },
            // lift function
            { Unit },
            \n            // Error FFI converter
            BackgroundRelayException.ErrorHandler,
        )
        } finally {
            `readCapability`.fill(0)
            manageCapabilityForLowering.fill(0)
        }
    }
""",
        "Kotlin aliased enrollment secret lowering",
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
    source = replace_once_upgrade(
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
        """        handleSuccess(callResult)
    }
    job.invokeOnCompletion { onCompletion?.invoke() }
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
}

internal inline fun<T, reified E: Throwable> uniffiTraitInterfaceCallAsyncWithError(
""",
        "Kotlin async callback protected registration",
    )
    source = replace_once(
        source,
        """internal inline fun<T, reified E: Throwable> uniffiTraitInterfaceCallAsyncWithError(
    crossinline makeCall: suspend () -> T,
    crossinline handleSuccess: (T) -> Unit,
    crossinline handleError: (UniffiRustCallStatus.ByValue) -> Unit,
    crossinline lowerError: (E) -> RustBuffer.ByValue,
    uniffiOutDroppedCallback: UniffiForeignFutureDroppedCallbackStruct,
) {
""",
        """internal inline fun<T, reified E: Throwable> uniffiTraitInterfaceCallAsyncWithError(
    crossinline makeCall: suspend () -> T,
    crossinline handleSuccess: (T) -> Unit,
    crossinline handleError: (UniffiRustCallStatus.ByValue) -> Unit,
    crossinline lowerError: (E) -> RustBuffer.ByValue,
    uniffiOutDroppedCallback: UniffiForeignFutureDroppedCallbackStruct,
    noinline onCompletion: (() -> Unit)? = null,
) {
""",
        "Kotlin fallible async callback completion parameter",
    )
    source = replace_once(
        source,
        """    val job = GlobalScope.launch coroutineBlock@ {
        // See the note in uniffiTraitInterfaceCallAsync for details on `handleSuccess` and
""",
        """    val job = GlobalScope.launch(
        start = kotlinx.coroutines.CoroutineStart.LAZY,
    ) coroutineBlock@ {
        // See the note in uniffiTraitInterfaceCallAsync for details on `handleSuccess` and
""",
        "Kotlin fallible pre-dispatch lazy callback job",
    )
    source = replace_once_upgrade(
        source,
        """        handleSuccess(callResult)
    }
    val handle = uniffiForeignFutureHandleMap.insert(job)
    uniffiOutDroppedCallback.uniffiSetValue(UniffiForeignFutureDroppedCallbackStruct(handle, uniffiForeignFutureDroppedCallbackImpl))
}

internal val uniffiForeignFutureHandleMap = UniffiHandleMap<Job>()
""",
        """        handleSuccess(callResult)
    }
    job.invokeOnCompletion { onCompletion?.invoke() }
    val handle = uniffiForeignFutureHandleMap.insert(job)
    uniffiOutDroppedCallback.uniffiSetValue(UniffiForeignFutureDroppedCallbackStruct(handle, uniffiForeignFutureDroppedCallbackImpl))
    job.start()
}

internal val uniffiForeignFutureHandleMap = UniffiHandleMap<Job>()
""",
        """        handleSuccess(callResult)
    }
    job.invokeOnCompletion { onCompletion?.invoke() }
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
}

internal val uniffiForeignFutureHandleMap = UniffiHandleMap<Job>()
""",
        "Kotlin fallible async callback protected registration",
    )
    source = replace_exact(
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
        3,
        "Kotlin secret-result success callbacks",
    )
    for method in ("write", "createIfAbsent"):
        source = replace_once_in_kotlin_secret_callback(
            source,
            method,
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
        source = replace_once_in_kotlin_secret_callback(
            source,
            method,
            """            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback
            )
""",
            """            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback,
                { secretValue.fill(0) },
            )
""",
            f"Kotlin {method} secret completion hook",
        )
    source = replace_once_in_kotlin_secret_callback(
        source,
        "compareAndSwap",
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
    source = replace_once_in_kotlin_secret_callback(
        source,
        "compareAndSwap",
        """            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback
            )
""",
        """            uniffiTraitInterfaceCallAsync(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                uniffiOutDroppedCallback,
                { secretValue.fill(0) },
            )
""",
        "Kotlin compareAndSwap secret completion hook",
    )
    source = replace_once_in_kotlin_callback(
        source,
        "AppRemoraLinkDeviceKeyBackend",
        "signMessage",
        """            val uniffiObj = FfiConverterTypeAppRemoraLinkDeviceKeyBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`signMessage`(
                    FfiConverterString.lift(`slot`),
                    FfiConverterTypeAppRelaySecretValue.lift(`message`),
                )
            }
""",
        """            val secretValue = FfiConverterTypeAppRelaySecretValue.lift(`message`)
            val uniffiObj = try {
                FfiConverterTypeAppRemoraLinkDeviceKeyBackend.handleMap.get(uniffiHandle)
            } catch (error: Throwable) {
                secretValue.fill(0)
                throw error
            }
            val makeCall = suspend { ->
                uniffiObj.`signMessage`(
                    FfiConverterString.lift(`slot`),
                    secretValue,
                )
            }
""",
        "Kotlin device-signing message callback",
    )
    source = replace_once_in_kotlin_callback(
        source,
        "AppRemoraLinkDeviceKeyBackend",
        "signMessage",
        """            uniffiTraitInterfaceCallAsyncWithError(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                { e: AppRemoraLinkDeviceKeyException -> FfiConverterTypeAppRemoraLinkDeviceKeyError.lower(e) },
                uniffiOutDroppedCallback
            )
""",
        """            uniffiTraitInterfaceCallAsyncWithError(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                { e: AppRemoraLinkDeviceKeyException -> FfiConverterTypeAppRemoraLinkDeviceKeyError.lower(e) },
                uniffiOutDroppedCallback,
                { secretValue.fill(0) },
            )
""",
        "Kotlin device-signing message completion hook",
    )
    source = replace_once_in_kotlin_callback(
        source,
        "AppRemoraLinkTransportIdentityBackend",
        "loadOrCreate",
        """            val uniffiObj = FfiConverterTypeAppRemoraLinkTransportIdentityBackend.handleMap.get(uniffiHandle)
            val makeCall = suspend { ->
                uniffiObj.`loadOrCreate`(
                    FfiConverterTypeAppRelaySecretValue.lift(`candidate`),
                )
            }
""",
        """            val secretValue = FfiConverterTypeAppRelaySecretValue.lift(`candidate`)
            val uniffiObj = try {
                FfiConverterTypeAppRemoraLinkTransportIdentityBackend.handleMap.get(uniffiHandle)
            } catch (error: Throwable) {
                secretValue.fill(0)
                throw error
            }
            val makeCall = suspend { ->
                uniffiObj.`loadOrCreate`(
                    secretValue,
                )
            }
""",
        "Kotlin transport-identity candidate callback",
    )
    source = replace_once_in_kotlin_callback(
        source,
        "AppRemoraLinkTransportIdentityBackend",
        "loadOrCreate",
        """            uniffiTraitInterfaceCallAsyncWithError(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                { e: AppRemoraLinkTransportIdentityException -> FfiConverterTypeAppRemoraLinkTransportIdentityError.lower(e) },
                uniffiOutDroppedCallback
            )
""",
        """            uniffiTraitInterfaceCallAsyncWithError(
                makeCall,
                uniffiHandleSuccess,
                uniffiHandleError,
                { e: AppRemoraLinkTransportIdentityException -> FfiConverterTypeAppRemoraLinkTransportIdentityError.lower(e) },
                uniffiOutDroppedCallback,
                { secretValue.fill(0) },
            )
""",
        "Kotlin transport-identity candidate completion hook",
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
    require_exact(
        source,
        "`message`: AppRelaySecretValue",
        1,
        "Kotlin device-signing message argument",
    )
    require_exact(
        source,
        "`candidate`: AppRelaySecretValue",
        1,
        "Kotlin transport-identity candidate argument",
    )
    require_exact(
        source,
        "public class AppRemoraLinkPairingCode private constructor(",
        1,
        "Kotlin reference pairing-code carrier",
    )
    require_exact(
        source,
        "public typealias AppRemoraLinkPairingCode = kotlin.ByteArray",
        0,
        "Kotlin absence of unbounded pairing-code alias",
    )
    require_exact(
        source,
        "public const val MAXIMUM_BYTE_COUNT: kotlin.Int = 4_128",
        1,
        "Kotlin pairing-code construction bound",
    )
    require_exact(
        source,
        "throw RemoraLinkException.InvalidPairingCode()",
        1,
        "Kotlin checked oversized pairing-code rejection",
    )
    require_exact(
        source,
        "public typealias FfiConverterTypeAppRemoraLinkPairingCode = "
        "FfiConverterByteArray",
        0,
        "Kotlin absence of raw pairing-code converter alias",
    )
    require_exact(
        source,
        "FfiConverterTypeAppRemoraLinkPairingCode.lower(`code`)",
        1,
        "Kotlin direct pairing-code lowering call",
    )
    require_exact(
        source,
        "FfiConverterOptionalTypeAppRemoraLinkPairingCode.lower(`code`)",
        1,
        "Kotlin optional pairing-code lowering call",
    )
    optional_pairing_converter_start = source.index(
        "public object FfiConverterOptionalTypeAppRemoraLinkPairingCode: "
        "FfiConverterRustBuffer<AppRemoraLinkPairingCode?> {"
    )
    optional_pairing_converter_end = source.index(
        "\n}", optional_pairing_converter_start
    ) + len("\n}")
    optional_pairing_converter = source[
        optional_pairing_converter_start:optional_pairing_converter_end
    ]
    require_exact(
        optional_pairing_converter,
        "value?.zeroize()",
        1,
        "Kotlin optional pairing-code carrier cleanup",
    )
    verify_kotlin_secret_callback_hardening(source)
    verify_kotlin_remora_link_callback_hardening(source)
    verify_kotlin_device_database_key_hardening(source)
    return source


def harden_kotlin(path: Path) -> None:
    source = harden_kotlin_source(path.read_text())
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
