import Network
import XCTest
@testable import Remora

@MainActor
final class ChatGPTOAuthTests: XCTestCase {
    func testAuthorizeURLUsesFixedLocalhostRedirect() throws {
        let url = try ChatGPTOAuth.buildAuthorizeURL(
            state: "state-123",
            codeChallenge: "challenge-456",
            redirectURI: "http://localhost:1455/auth/callback"
        )
        let components = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
        let query = Dictionary(
            uniqueKeysWithValues: (components.queryItems ?? []).map { ($0.name, $0.value ?? "") }
        )

        XCTAssertEqual(components.scheme, "https")
        XCTAssertEqual(components.host, "auth.openai.com")
        XCTAssertEqual(components.path, "/oauth/authorize")
        XCTAssertEqual(query["redirect_uri"], "http://localhost:1455/auth/callback")
        XCTAssertEqual(query["scope"], "openid profile email offline_access")
        XCTAssertEqual(query["codex_cli_simplified_flow"], "true")
    }

    func testAuthorizeURLLoggingRedactsStateAndCodeChallenge() throws {
        let state = "oauth-state-secret"
        let codeChallenge = "pkce-challenge-secret"
        let url = try ChatGPTOAuth.buildAuthorizeURL(
            state: state,
            codeChallenge: codeChallenge,
            redirectURI: "http://localhost:1455/auth/callback"
        )

        let sanitizedURL = try XCTUnwrap(
            URL(string: ChatGPTOAuth.sanitizedAuthorizeURLForLogging(url))
        )
        let components = try XCTUnwrap(
            URLComponents(url: sanitizedURL, resolvingAgainstBaseURL: false)
        )
        let query = Dictionary(
            uniqueKeysWithValues: (components.queryItems ?? []).map { ($0.name, $0.value ?? "") }
        )

        XCTAssertEqual(query["state"], "<redacted>")
        XCTAssertEqual(query["code_challenge"], "<redacted>")
        XCTAssertFalse(sanitizedURL.absoluteString.contains(state))
        XCTAssertFalse(sanitizedURL.absoluteString.contains(codeChallenge))
    }

    func testValidateCallbackURLAcceptsLoopbackCallback() throws {
        let url = try XCTUnwrap(URL(string: "http://127.0.0.1:1455/auth/callback?code=abc&state=xyz"))

        let components = try ChatGPTOAuth.validateCallbackURL(url)

        XCTAssertEqual(components.path, "/auth/callback")
        XCTAssertEqual(
            Dictionary(uniqueKeysWithValues: (components.queryItems ?? []).map { ($0.name, $0.value ?? "") })["code"],
            "abc"
        )
    }

    func testValidateCallbackURLRejectsCustomSchemeCallbacks() throws {
        let url = try XCTUnwrap(URL(string: "remoraauth://auth/callback?code=abc&state=xyz"))

        XCTAssertThrowsError(try ChatGPTOAuth.validateCallbackURL(url))
    }

    func testCallbackListenerBindsOnlyConfiguredLoopbackAndStartsWithFourPermits() throws {
        let port = try XCTUnwrap(NWEndpoint.Port(rawValue: 1455))
        let listener = try ChatGPTOAuth.callbackListener(bindHost: "127.0.0.1", port: port)
        defer { listener.cancel() }

        XCTAssertEqual(
            listener.parameters.requiredLocalEndpoint,
            .hostPort(host: "127.0.0.1", port: port)
        )
        XCTAssertEqual(listener.newConnectionLimit, 4)
    }

    func testCallbackConnectionPermitFinishesExactlyOnceAcrossConcurrentCallers() async {
        let counter = LockedTestCounter()
        let permit = CallbackConnectionPermit {
            counter.increment()
        }

        await withTaskGroup(of: Void.self) { group in
            for _ in 0..<32 {
                group.addTask {
                    permit.finish()
                }
            }
        }
        permit.finish()

        XCTAssertEqual(counter.value, 1)
    }

    func testCallbackReceiveDecisionWaitsForCompleteHeaderThenProcessesIt() {
        let first = Data("GET /auth/callback?code=test HTTP/1.1\r\nHost: local".utf8)
        XCTAssertEqual(
            ChatGPTOAuth.callbackReceiveDecision(
                buffer: Data(),
                incoming: first,
                isComplete: false
            ),
            .receive(first)
        )

        let final = Data("host\r\n\r\n".utf8)
        var complete = first
        complete.append(final)
        XCTAssertEqual(
            ChatGPTOAuth.callbackReceiveDecision(
                buffer: first,
                incoming: final,
                isComplete: false
            ),
            .process(complete)
        )
    }

    func testCallbackReceiveDecisionAcceptsExactLimitAndRejectsOneByteOver() {
        let terminator = CallbackReceiveDecision.headerTerminator
        var exact = Data(repeating: 65, count: ChatGPTOAuth.callbackRequestMaxBytes - terminator.count)
        exact.append(terminator)
        XCTAssertEqual(
            ChatGPTOAuth.callbackReceiveDecision(
                buffer: Data(),
                incoming: exact,
                isComplete: false
            ),
            .process(exact)
        )

        let fullBuffer = Data(repeating: 65, count: ChatGPTOAuth.callbackRequestMaxBytes)
        XCTAssertEqual(
            ChatGPTOAuth.callbackReceiveDecision(
                buffer: fullBuffer,
                incoming: Data([66]),
                isComplete: false
            ),
            .reject
        )
    }

    func testCallbackReceiveDecisionRejectsEOFWithoutHeaderTerminator() {
        XCTAssertEqual(
            ChatGPTOAuth.callbackReceiveDecision(
                buffer: Data(),
                incoming: Data("GET /auth/callback HTTP/1.1\r\n".utf8),
                isComplete: true
            ),
            .reject
        )
    }

    func testCallbackRequestURLAcceptsStrictRequestAndPreservesQuery() throws {
        let request = Data(
            "GET /auth/callback?code=test-code&state=test-state HTTP/1.1\r\nHost: localhost\r\n\r\n".utf8
        )

        let url = try ChatGPTOAuth.callbackRequestURL(
            from: request,
            publicHost: "localhost",
            port: 1455,
            path: "/auth/callback"
        )

        XCTAssertEqual(url.scheme, "http")
        XCTAssertEqual(url.host, "localhost")
        XCTAssertEqual(url.port, 1455)
        XCTAssertEqual(url.path, "/auth/callback")
        XCTAssertEqual(url.query, "code=test-code&state=test-state")
    }

    func testCallbackRequestURLRejectsInvalidUTF8AndMalformedRequestLines() {
        var invalidUTF8 = Data("GET /auth/callback HTTP/1.1\r\nX: ".utf8)
        invalidUTF8.append(0xFF)
        invalidUTF8.append(Data("\r\n\r\n".utf8))

        let invalidRequests = [
            invalidUTF8,
            Data("POST /auth/callback HTTP/1.1\r\n\r\n".utf8),
            Data("GET https://localhost/auth/callback HTTP/1.1\r\n\r\n".utf8),
            Data("GET //localhost/auth/callback HTTP/1.1\r\n\r\n".utf8),
            Data("GET  /auth/callback HTTP/1.1\r\n\r\n".utf8),
            Data("GET /auth/callback HTTP/2\r\n\r\n".utf8),
            Data("GET /wrong HTTP/1.1\r\n\r\n".utf8),
            Data("GET /auth/callback HTTP/1.1\r\n".utf8)
        ]

        for request in invalidRequests {
            XCTAssertThrowsError(
                try ChatGPTOAuth.callbackRequestURL(
                    from: request,
                    publicHost: "localhost",
                    port: 1455,
                    path: "/auth/callback"
                )
            ) { error in
                guard case ChatGPTOAuthError.invalidCallbackURL = error else {
                    return XCTFail("Expected invalidCallbackURL")
                }
            }
        }
    }

    func testCallbackQueryItemsRejectDuplicateKeysInsteadOfTrapping() throws {
        let url = try XCTUnwrap(URL(string: "http://localhost:1455/auth/callback?code=first&code=second&state=xyz"))
        let components = try ChatGPTOAuth.validateCallbackURL(url)

        XCTAssertThrowsError(try ChatGPTOAuth.callbackQueryItems(from: components)) { error in
            guard case ChatGPTOAuthError.invalidCallbackURL = error else {
                return XCTFail("Expected invalidCallbackURL, got \(error)")
            }
        }
    }

    func testTransientKeychainAvailabilityDetectionMatchesRelevantStatuses() {
        XCTAssertTrue(ChatGPTOAuthError.keychain(errSecInteractionNotAllowed).isTransientKeychainAvailabilityFailure)
        XCTAssertTrue(ChatGPTOAuthError.keychain(errSecNotAvailable).isTransientKeychainAvailabilityFailure)
        XCTAssertFalse(ChatGPTOAuthError.keychain(errSecItemNotFound).isTransientKeychainAvailabilityFailure)
        XCTAssertFalse(ChatGPTOAuthError.missingStoredTokens.isTransientKeychainAvailabilityFailure)
    }

    func testTokenBundlePreservesExistingRefreshTokenWhenRefreshResponseOmitsIt() throws {
        let idToken = jwt(claims: [
            "chatgpt_account_id": "acct_123",
            "chatgpt_plan_type": "plus"
        ])
        let accessToken = jwt(claims: [
            "chatgpt_account_id": "acct_123"
        ])

        let bundle = try ChatGPTOAuth.tokenBundle(
            from: [
                "access_token": accessToken,
                "id_token": idToken
            ],
            statusCode: 200,
            fallbackRefreshToken: "refresh_123"
        )

        XCTAssertEqual(bundle.refreshToken, "refresh_123")
        XCTAssertEqual(bundle.accountID, "acct_123")
        XCTAssertEqual(bundle.planType, "plus")
    }

    func testPersistValidatedRefreshSavesMatchingAccounts() throws {
        let refreshed = tokenBundle(accountID: "acct_expected")
        var saveCount = 0

        let result = try ChatGPTOAuth.persistValidatedRefresh(
            refreshed,
            expectedAccountID: "acct_expected",
            storedAccountID: "acct_expected"
        ) { saved in
            XCTAssertEqual(saved, refreshed)
            saveCount += 1
        }

        XCTAssertEqual(result, refreshed)
        XCTAssertEqual(saveCount, 1)
    }

    func testPersistValidatedRefreshRejectsRefreshedAccountMismatchBeforeSaving() {
        let refreshed = tokenBundle(accountID: "acct_refreshed")
        var saveCount = 0

        XCTAssertThrowsError(
            try ChatGPTOAuth.persistValidatedRefresh(
                refreshed,
                expectedAccountID: "acct_expected",
                storedAccountID: "acct_expected"
            ) { _ in
                saveCount += 1
            }
        ) { error in
            guard case ChatGPTOAuthError.refreshAccountMismatch = error else {
                return XCTFail("Expected refreshAccountMismatch")
            }
        }
        XCTAssertEqual(saveCount, 0)
    }

    func testPersistValidatedRefreshRejectsStoredAccountMismatchBeforeSaving() {
        let refreshed = tokenBundle(accountID: "acct_expected")
        var saveCount = 0

        XCTAssertThrowsError(
            try ChatGPTOAuth.persistValidatedRefresh(
                refreshed,
                expectedAccountID: "acct_expected",
                storedAccountID: "acct_stored"
            ) { _ in
                saveCount += 1
            }
        ) { error in
            guard case ChatGPTOAuthError.refreshAccountMismatch = error else {
                return XCTFail("Expected refreshAccountMismatch")
            }
        }
        XCTAssertEqual(saveCount, 0)
    }

    func testPersistValidatedRefreshAllowsMissingExpectedAccount() throws {
        let refreshed = tokenBundle(accountID: "acct_refreshed")
        var saveCount = 0

        let nilExpected = try ChatGPTOAuth.persistValidatedRefresh(
            refreshed,
            expectedAccountID: nil,
            storedAccountID: "acct_stored"
        ) { _ in
            saveCount += 1
        }
        XCTAssertEqual(nilExpected, refreshed)
        XCTAssertEqual(saveCount, 1)

        let emptyExpected = try ChatGPTOAuth.persistValidatedRefresh(
            refreshed,
            expectedAccountID: "",
            storedAccountID: "acct_stored"
        ) { _ in
            saveCount += 1
        }
        XCTAssertEqual(emptyExpected, refreshed)
        XCTAssertEqual(saveCount, 2)
    }

    func testOAuthErrorBodyOmitsNonJSONTokenShapedResponse() {
        let leakedToken = "sk-proj-this-must-never-reach-diagnostics"

        let preview = ChatGPTOAuth.oauthErrorResponseMetadata("Bearer \(leakedToken)")

        XCTAssertEqual(preview, "<non-JSON response omitted>")
        XCTAssertFalse(preview.contains(leakedToken))
    }

    func testOAuthErrorBodyEmitsStructureWithoutAnyResponseValues() {
        let leakedRefreshToken = "refresh-token-this-must-never-reach-diagnostics"
        let leakedBearerToken = "sk-proj-this-must-also-be-redacted"
        let leakedBenignValue = "sk-proj-echoed-under-a-benign-key"
        let leakedNestedKey = "sk-proj-secret-used-as-a-json-key"
        let response = """
        {
          "error": "invalid_grant",
          "message": "\(leakedBenignValue)",
          "details": [
            {"refresh_token": "\(leakedRefreshToken)"},
            {"message": "Bearer \(leakedBearerToken)"},
            {"\(leakedNestedKey)": "invalid"}
          ]
        }
        """

        let preview = ChatGPTOAuth.oauthErrorResponseMetadata(response)

        XCTAssertTrue(preview.contains("JSON object response omitted"))
        XCTAssertFalse(preview.contains("invalid_grant"))
        XCTAssertFalse(preview.contains(leakedBenignValue))
        XCTAssertFalse(preview.contains(leakedRefreshToken))
        XCTAssertFalse(preview.contains(leakedBearerToken))
        XCTAssertFalse(preview.contains(leakedNestedKey))
    }

    func testOAuthResponseMetadataDoesNotExposeCredentialShapedTopLevelKeys() {
        let leakedKey = "sk-proj-secret-used-as-a-top-level-key"
        let response = "{\"error\":\"invalid_grant\",\"\(leakedKey)\":\"invalid\"}"
        let data = Data(response.utf8)

        let keys = ChatGPTOAuth.jsonObjectKeys(data)
        let preview = ChatGPTOAuth.oauthErrorResponseMetadata(response)

        XCTAssertEqual(keys, ["<other>", "error"])
        XCTAssertFalse(keys.joined().contains(leakedKey))
        XCTAssertFalse(preview.contains(leakedKey))
    }

    func testOAuthResponseMetadataRejectsJSONWithTrailingSecretMaterial() {
        let leakedToken = "sk-proj-trailing-material-must-be-omitted"
        let responses = [
            "{\"error\":\"invalid_grant\"} \(leakedToken)",
            "[{\"error\":\"invalid_grant\"}] \(leakedToken)",
            "\"invalid_grant\" \(leakedToken)"
        ]

        for response in responses {
            let preview = ChatGPTOAuth.oauthErrorResponseMetadata(response)

            XCTAssertEqual(preview, "<non-JSON response omitted>")
            XCTAssertFalse(preview.contains(leakedToken))
        }
    }

    func testOAuthResponseMetadataRejectsLenientJSONExtensions() {
        let responses = [
            "{error:\"invalid_grant\"}",
            "{\"error\":'invalid_grant'}",
            "{\"error\":\"invalid_grant\",}",
            "[1,]"
        ]

        for response in responses {
            XCTAssertEqual(
                ChatGPTOAuth.oauthErrorResponseMetadata(response),
                "<non-JSON response omitted>"
            )
        }
    }

    func testOAuthResponseMetadataRejectsCaseVariantLiteralsAndRawStringControls() {
        let responses = [
            "{\"error\":TRUE}",
            "[False]",
            "{\"nested\":{\"value\":NULL}}",
            "{\"error\":\"line one\nline two\"}",
            "{\"nested\":[\"before\u{0001}after\"]}"
        ]

        for response in responses {
            XCTAssertEqual(
                ChatGPTOAuth.oauthErrorResponseMetadata(response),
                "<non-JSON response omitted>"
            )
            XCTAssertTrue(ChatGPTOAuth.jsonObjectKeys(Data(response.utf8)).isEmpty)
        }
    }

    private func jwt(claims: [String: String]) -> String {
        let header = ["alg": "none", "typ": "JWT"]
        let encoder = JSONEncoder()
        let headerData = try! encoder.encode(header)
        let payloadData = try! encoder.encode(claims)
        return [
            headerData.base64URLEncodedString(),
            payloadData.base64URLEncodedString(),
            ""
        ].joined(separator: ".")
    }

    private func tokenBundle(accountID: String) -> ChatGPTOAuthTokenBundle {
        ChatGPTOAuthTokenBundle(
            accessToken: "access_token",
            idToken: "id_token",
            refreshToken: "refresh_token",
            accountID: accountID,
            planType: nil
        )
    }
}

private final class LockedTestCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var storage = 0

    var value: Int {
        lock.withLock { storage }
    }

    func increment() {
        lock.withLock {
            storage += 1
        }
    }
}

final class LLogTests: XCTestCase {
    func testReleaseRenderingOnlyIncludesAllowlistedOperationalMetadata() {
        let rendered = LLog.render(
            message: "request failed",
            fields: [
                "operation": "send_message",
                "error_domain": "NSURLErrorDomain",
                "error_code": -1009,
                "status": 503,
                "server_id": "server-secret",
                "thread_id": "thread-secret",
                "cursor": "cursor-secret",
                "state": "oauth-state-secret",
                "error_description": "raw error secret"
            ],
            payloadJson: #"{"token":"payload-secret"}"#,
            policy: .release
        )

        XCTAssertEqual(
            rendered,
            #"request failed fields={"error_code":-1009,"error_domain":"NSURLErrorDomain","operation":"send_message","status":503}"#
        )
        XCTAssertFalse(rendered.contains("server-secret"))
        XCTAssertFalse(rendered.contains("thread-secret"))
        XCTAssertFalse(rendered.contains("cursor-secret"))
        XCTAssertFalse(rendered.contains("oauth-state-secret"))
        XCTAssertFalse(rendered.contains("raw error secret"))
        XCTAssertFalse(rendered.contains("payload-secret"))
    }

    func testReleaseRenderingRejectsUnstructuredValuesForStringMetadata() {
        let rendered = LLog.render(
            message: "request failed",
            fields: [
                "operation": "send message for account@example.com",
                "error_domain": "secret\nvalue",
                "error_code": 7
            ],
            payloadJson: nil,
            policy: .release
        )

        XCTAssertEqual(rendered, #"request failed fields={"error_code":7}"#)
    }

    func testDiagnosticRenderingPreservesFieldsAndPayload() {
        let rendered = LLog.render(
            message: "request failed",
            fields: [
                "thread_id": "thread-debug",
                "error_description": "debug description"
            ],
            payloadJson: #"{"detail":"debug payload"}"#,
            policy: .diagnostic
        )

        XCTAssertTrue(rendered.contains("thread-debug"))
        XCTAssertTrue(rendered.contains("debug description"))
        XCTAssertTrue(rendered.contains("debug payload"))
    }
}

private extension Data {
    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
