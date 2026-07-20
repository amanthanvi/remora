package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatGPTOAuthRedactionTest {
    @Test
    fun nonJsonTokenShapedResponseIsOmitted() {
        val leakedToken = "sk-proj-this-must-never-reach-diagnostics"

        val preview = ChatGPTOAuth.oauthErrorResponseMetadata("Bearer $leakedToken")

        assertEquals("<non-JSON response omitted>", preview)
        assertFalse(preview.contains(leakedToken))
    }

    @Test
    fun jsonDiagnosticsEmitStructureWithoutAnyResponseValues() {
        val leakedRefreshToken = "refresh-token-this-must-never-reach-diagnostics"
        val leakedBearerToken = "sk-proj-this-must-also-be-redacted"
        val leakedBenignValue = "sk-proj-echoed-under-a-benign-key"
        val leakedNestedKey = "sk-proj-secret-used-as-a-json-key"
        val response = """
            {
              "error": "invalid_grant",
              "message": "$leakedBenignValue",
              "details": [
                {"refresh_token": "$leakedRefreshToken"},
                {"message": "Bearer $leakedBearerToken"},
                {"$leakedNestedKey": "invalid"}
              ]
            }
        """.trimIndent()

        val preview = ChatGPTOAuth.oauthErrorResponseMetadata(response)

        assertTrue(preview.contains("JSON object response omitted"))
        assertFalse(preview.contains("invalid_grant"))
        assertFalse(preview.contains(leakedBenignValue))
        assertFalse(preview.contains(leakedRefreshToken))
        assertFalse(preview.contains(leakedBearerToken))
        assertFalse(preview.contains(leakedNestedKey))
    }

    @Test
    fun responseMetadataDoesNotExposeCredentialShapedTopLevelKeys() {
        val leakedKey = "sk-proj-secret-used-as-a-top-level-key"
        val response = """{"error":"invalid_grant","$leakedKey":"invalid"}"""

        val keys = ChatGPTOAuth.jsonObjectKeys(response)
        val preview = ChatGPTOAuth.oauthErrorResponseMetadata(response)

        assertEquals(listOf("<other>", "error"), keys)
        assertFalse(keys.joinToString().contains(leakedKey))
        assertFalse(preview.contains(leakedKey))
    }

    @Test
    fun jsonWithTrailingSecretMaterialIsRejected() {
        val leakedToken = "sk-proj-trailing-material-must-be-omitted"
        val responses = listOf(
            """{"error":"invalid_grant"} $leakedToken""",
            """[{"error":"invalid_grant"}] $leakedToken""",
            """"invalid_grant" $leakedToken""",
        )

        responses.forEach { response ->
            val preview = ChatGPTOAuth.oauthErrorResponseMetadata(response)

            assertEquals("<non-JSON response omitted>", preview)
            assertFalse(preview.contains(leakedToken))
        }
    }

    @Test
    fun lenientJsonExtensionsAreRejected() {
        val responses = listOf(
            """{error:"invalid_grant"}""",
            """{"error":'invalid_grant'}""",
            """{"error":"invalid_grant",}""",
            """[1,]""",
        )

        responses.forEach { response ->
            assertEquals(
                "<non-JSON response omitted>",
                ChatGPTOAuth.oauthErrorResponseMetadata(response),
            )
        }
    }

    @Test
    fun caseVariantLiteralsAndRawStringControlsAreRejected() {
        val responses = listOf(
            """{"error":TRUE}""",
            """[False]""",
            """{"nested":{"value":NULL}}""",
            "{\"error\":\"line one\nline two\"}",
            "{\"nested\":[\"before\u0001after\"]}",
        )

        responses.forEach { response ->
            assertEquals(
                "<non-JSON response omitted>",
                ChatGPTOAuth.oauthErrorResponseMetadata(response),
            )
            assertTrue(ChatGPTOAuth.jsonObjectKeys(response).isEmpty())
        }
    }
}
