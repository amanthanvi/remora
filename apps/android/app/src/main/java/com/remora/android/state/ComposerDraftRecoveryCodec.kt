package com.remora.android.state

import java.util.Base64
import org.json.JSONArray
import org.json.JSONObject
import uniffi.codex_mobile_client.*

// V1 is an app-owned storage contract. No generated RPC serializer or UniFFI buffer is persisted.
private data class RecoveryDestinationV1(val kind: String, val server: String?, val location: String?)
private data class RecoveryFileV1(val label: String, val path: String)
private data class RecoveryImageV1(val mime: String, val base64: String)
private data class RecoveryInputV1(val kind: String, val value: String, val name: String?, val ranges: List<Pair<ULong, ULong>>)
private data class RecoveryApprovalV1(val kind: String, val flags: List<Boolean>)
private data class RecoveryAccessV1(val kind: String, val defaults: Boolean, val roots: List<String>)
private data class RecoverySandboxV1(
    val kind: String, val access: RecoveryAccessV1?, val roots: List<String>,
    val network: Boolean, val networkMode: String?, val excludeEnv: Boolean, val excludeTmp: Boolean,
)
private data class RecoveryPayloadV1(
    val text: String, val inputs: List<RecoveryInputV1>, val files: List<RecoveryFileV1>,
    val approval: RecoveryApprovalV1?, val sandbox: RecoverySandboxV1?, val model: String?,
    val effort: String?, val tier: String?,
)
private data class RecoveryToolV1(val name: String, val description: String, val schema: String, val deferred: Boolean)
private data class RecoveryStartV1(
    val runtime: String?, val model: String?, val cwd: String?, val approval: RecoveryApprovalV1?,
    val sandbox: String?, val instructions: String?, val history: Boolean,
    val tools: List<RecoveryToolV1>?, val ephemeral: Boolean?,
)
private data class RecoveryEntryV1(
    val id: Long, val destination: RecoveryDestinationV1, val text: String,
    val image: RecoveryImageV1?, val files: List<RecoveryFileV1>, val status: String,
    val payload: RecoveryPayloadV1?, val start: RecoveryStartV1?, val created: RecoveryDestinationV1?,
)

internal object ComposerDraftRecoveryCodec {
    fun encode(entries: List<RecoverableComposerDraft>): ByteArray = obj(
        "version" to 1, "entries" to entries.map { it.toV1().json() },
    ).toString().toByteArray(Charsets.UTF_8)

    fun decode(bytes: ByteArray): List<RecoverableComposerDraft> {
        require(bytes.size <= AtomicComposerDraftRecoveryPersistence.MAX_ARCHIVE_BYTES)
        val root = JSONObject(bytes.toString(Charsets.UTF_8))
        require(root.getInt("version") == 1) { "Unsupported draft recovery version" }
        val entries = root.objects("entries").map { entryV1(it).native() }
        require(entries.all { it.id > 0 } && entries.map { it.id }.distinct().size == entries.size)
        return entries
    }
}

private fun RecoverableComposerDraft.toV1() = RecoveryEntryV1(
    id, when (val d = destination) {
        is ComposerDraftDestination.Conversation -> RecoveryDestinationV1("conversation", d.key.serverId, d.key.threadId)
        is ComposerDraftDestination.Home -> RecoveryDestinationV1("home", d.serverId, d.cwd)
    }, draft.text, draft.attachment?.let { RecoveryImageV1(it.mimeType, Base64.getEncoder().encodeToString(it.data)) },
    draft.fileAttachments.map { RecoveryFileV1(it.label, it.path) }, status.name,
    payload?.let { p -> RecoveryPayloadV1(p.text, p.additionalInputs.map(::inputV1),
        p.fileAttachments.map { RecoveryFileV1(it.label, it.path) }, p.approvalPolicy?.let(::approvalV1),
        p.sandboxPolicy?.let(::sandboxV1), p.model, p.reasoningEffort?.name, p.serviceTier?.name) },
    threadStartRequest?.let { s -> RecoveryStartV1(s.agentRuntimeKind, s.model, s.cwd,
        s.approvalPolicy?.let(::approvalV1), s.sandbox?.name, s.developerInstructions,
        s.persistExtendedHistory, s.dynamicTools?.map { RecoveryToolV1(it.name, it.description, it.inputSchemaJson, it.deferLoading) }, s.ephemeral) },
    createdThreadKey?.let { RecoveryDestinationV1("conversation", it.serverId, it.threadId) },
)

private fun RecoveryEntryV1.native(): RecoverableComposerDraft = RecoverableComposerDraft(
    id, when (destination.kind) {
        "conversation" -> ComposerDraftDestination.Conversation(ThreadKey(checkNotNull(destination.server), checkNotNull(destination.location)))
        "home" -> ComposerDraftDestination.Home(destination.server, destination.location)
        else -> error("Unsupported draft destination")
    }, AppModel.ComposerDraft(text, image?.let { ComposerImageAttachment(Base64.getDecoder().decode(it.base64), it.mime) },
        files.map { ComposerFileAttachment(it.label, it.path) }), ComposerDraftRecoveryStatus.valueOf(status),
    payload?.let { p -> AppComposerPayload(p.text, p.inputs.map { it.native() },
        p.files.map { ComposerFileAttachment(it.label, it.path) }, p.approval?.native(), p.sandbox?.native(),
        p.model, p.effort?.let(ReasoningEffort::valueOf), p.tier?.let(ServiceTier::valueOf)) },
    start?.let { s -> AppStartThreadRequest(s.runtime, s.model, s.cwd, s.approval?.native(),
        s.sandbox?.let(AppSandboxMode::valueOf), s.instructions, s.history,
        s.tools?.map { AppDynamicToolSpec(it.name, it.description, it.schema, it.deferred) }, s.ephemeral) },
    created?.let { check(it.kind == "conversation"); ThreadKey(checkNotNull(it.server), checkNotNull(it.location)) },
)

private fun inputV1(input: AppUserInput): RecoveryInputV1 = when (input) {
    is AppUserInput.Text -> RecoveryInputV1("text", input.text, null,
        input.textElements.map { it.byteRange.start to it.byteRange.end })
    is AppUserInput.Image -> RecoveryInputV1("image", input.url, null, emptyList())
    is AppUserInput.LocalImage -> RecoveryInputV1("localImage", input.path.value, null, emptyList())
    is AppUserInput.Skill -> RecoveryInputV1("skill", input.path.value, input.name, emptyList())
    is AppUserInput.Mention -> RecoveryInputV1("mention", input.path, input.name, emptyList())
}

private fun RecoveryInputV1.native(): AppUserInput = when (kind) {
    "text" -> AppUserInput.Text(value, ranges.map { AppTextElement(AppByteRange(it.first, it.second)) })
    "image" -> AppUserInput.Image(value)
    "localImage" -> AppUserInput.LocalImage(AbsolutePath(value))
    "skill" -> AppUserInput.Skill(checkNotNull(name), AbsolutePath(value))
    "mention" -> AppUserInput.Mention(checkNotNull(name), value)
    else -> error("Unsupported draft input")
}

private fun approvalV1(policy: AppAskForApproval): RecoveryApprovalV1 = when (policy) {
    AppAskForApproval.Never -> RecoveryApprovalV1("never", emptyList())
    AppAskForApproval.OnFailure -> RecoveryApprovalV1("onFailure", emptyList())
    AppAskForApproval.OnRequest -> RecoveryApprovalV1("onRequest", emptyList())
    AppAskForApproval.UnlessTrusted -> RecoveryApprovalV1("unlessTrusted", emptyList())
    is AppAskForApproval.Granular -> RecoveryApprovalV1("granular", listOf(policy.sandboxApproval,
        policy.rules, policy.skillApproval, policy.requestPermissions, policy.mcpElicitations))
}

private fun RecoveryApprovalV1.native(): AppAskForApproval = when (kind) {
    "never" -> AppAskForApproval.Never
    "onFailure" -> AppAskForApproval.OnFailure
    "onRequest" -> AppAskForApproval.OnRequest
    "unlessTrusted" -> AppAskForApproval.UnlessTrusted
    "granular" -> { require(flags.size == 5); AppAskForApproval.Granular(flags[0], flags[1], flags[2], flags[3], flags[4]) }
    else -> error("Unsupported draft approval")
}

private fun accessV1(access: AppReadOnlyAccess): RecoveryAccessV1 = when (access) {
    AppReadOnlyAccess.FullAccess -> RecoveryAccessV1("full", false, emptyList())
    is AppReadOnlyAccess.Restricted -> RecoveryAccessV1("restricted", access.includePlatformDefaults, access.readableRoots.map { it.value })
}

private fun RecoveryAccessV1.native(): AppReadOnlyAccess = when (kind) {
    "full" -> AppReadOnlyAccess.FullAccess
    "restricted" -> AppReadOnlyAccess.Restricted(defaults, roots.map(::AbsolutePath))
    else -> error("Unsupported draft read access")
}

private fun sandboxV1(policy: AppSandboxPolicy): RecoverySandboxV1 = when (policy) {
    AppSandboxPolicy.DangerFullAccess -> RecoverySandboxV1("full", null, emptyList(), false, null, false, false)
    is AppSandboxPolicy.ReadOnly -> RecoverySandboxV1("readOnly", accessV1(policy.access), emptyList(), policy.networkAccess, null, false, false)
    is AppSandboxPolicy.ExternalSandbox -> RecoverySandboxV1("external", null, emptyList(), false, policy.networkAccess.name, false, false)
    is AppSandboxPolicy.WorkspaceWrite -> RecoverySandboxV1("workspace", accessV1(policy.readOnlyAccess),
        policy.writableRoots.map { it.value }, policy.networkAccess, null, policy.excludeTmpdirEnvVar, policy.excludeSlashTmp)
}

private fun RecoverySandboxV1.native(): AppSandboxPolicy = when (kind) {
    "full" -> AppSandboxPolicy.DangerFullAccess
    "readOnly" -> AppSandboxPolicy.ReadOnly(checkNotNull(access).native(), network)
    "external" -> AppSandboxPolicy.ExternalSandbox(AppNetworkAccess.valueOf(checkNotNull(networkMode)))
    "workspace" -> AppSandboxPolicy.WorkspaceWrite(roots.map(::AbsolutePath), checkNotNull(access).native(), network, excludeEnv, excludeTmp)
    else -> error("Unsupported draft sandbox")
}

private fun obj(vararg fields: Pair<String, Any?>): JSONObject = JSONObject().apply {
    fields.forEach { (key, value) -> put(key, if (value is List<*>) JSONArray(value) else value ?: JSONObject.NULL) }
}
private fun JSONObject.stringOrNull(key: String): String? = if (isNull(key)) null else getString(key)
private fun JSONObject.objectOrNull(key: String): JSONObject? = if (isNull(key)) null else getJSONObject(key)
private fun JSONObject.objects(key: String): List<JSONObject> = getJSONArray(key).let { a -> List(a.length()) { a.getJSONObject(it) } }
private fun JSONObject.strings(key: String): List<String> = getJSONArray(key).let { a -> List(a.length()) { a.getString(it) } }
private fun JSONObject.booleans(key: String): List<Boolean> = getJSONArray(key).let { a -> List(a.length()) { a.getBoolean(it) } }

private fun RecoveryDestinationV1.json() = obj("kind" to kind, "server" to server, "location" to location)
private fun destinationV1(j: JSONObject) = RecoveryDestinationV1(j.getString("kind"), j.stringOrNull("server"), j.stringOrNull("location"))
private fun RecoveryFileV1.json() = obj("label" to label, "path" to path)
private fun fileV1(j: JSONObject) = RecoveryFileV1(j.getString("label"), j.getString("path"))
private fun RecoveryApprovalV1.json() = obj("kind" to kind, "flags" to flags)
private fun approvalV1(j: JSONObject) = RecoveryApprovalV1(j.getString("kind"), j.booleans("flags"))
private fun RecoveryAccessV1.json() = obj("kind" to kind, "defaults" to defaults, "roots" to roots)
private fun accessV1(j: JSONObject) = RecoveryAccessV1(j.getString("kind"), j.getBoolean("defaults"), j.strings("roots"))
private fun RecoverySandboxV1.json() = obj("kind" to kind, "access" to access?.json(), "roots" to roots,
    "network" to network, "networkMode" to networkMode, "excludeEnv" to excludeEnv, "excludeTmp" to excludeTmp)
private fun sandboxV1(j: JSONObject) = RecoverySandboxV1(j.getString("kind"), j.objectOrNull("access")?.let(::accessV1),
    j.strings("roots"), j.getBoolean("network"), j.stringOrNull("networkMode"), j.getBoolean("excludeEnv"), j.getBoolean("excludeTmp"))
private fun RecoveryInputV1.json() = obj("kind" to kind, "value" to value, "name" to name,
    "ranges" to ranges.map { obj("start" to it.first.toString(), "end" to it.second.toString()) })
private fun inputV1(j: JSONObject) = RecoveryInputV1(j.getString("kind"), j.getString("value"), j.stringOrNull("name"),
    j.objects("ranges").map { it.getString("start").toULong() to it.getString("end").toULong() })
private fun RecoveryPayloadV1.json() = obj("text" to text, "inputs" to inputs.map { it.json() },
    "files" to files.map { it.json() }, "approval" to approval?.json(), "sandbox" to sandbox?.json(),
    "model" to model, "effort" to effort, "tier" to tier)
private fun payloadV1(j: JSONObject) = RecoveryPayloadV1(j.getString("text"), j.objects("inputs").map(::inputV1),
    j.objects("files").map(::fileV1), j.objectOrNull("approval")?.let(::approvalV1), j.objectOrNull("sandbox")?.let(::sandboxV1),
    j.stringOrNull("model"), j.stringOrNull("effort"), j.stringOrNull("tier"))
private fun RecoveryToolV1.json() = obj("name" to name, "description" to description, "schema" to schema, "deferred" to deferred)
private fun toolV1(j: JSONObject) = RecoveryToolV1(j.getString("name"), j.getString("description"), j.getString("schema"), j.getBoolean("deferred"))
private fun RecoveryStartV1.json() = obj("runtime" to runtime, "model" to model, "cwd" to cwd,
    "approval" to approval?.json(), "sandbox" to sandbox, "instructions" to instructions,
    "history" to history, "tools" to tools?.map { it.json() }, "ephemeral" to ephemeral)
private fun startV1(j: JSONObject) = RecoveryStartV1(j.stringOrNull("runtime"), j.stringOrNull("model"), j.stringOrNull("cwd"),
    j.objectOrNull("approval")?.let(::approvalV1), j.stringOrNull("sandbox"), j.stringOrNull("instructions"),
    j.getBoolean("history"), if (j.isNull("tools")) null else j.objects("tools").map(::toolV1),
    if (j.isNull("ephemeral")) null else j.getBoolean("ephemeral"))
private fun RecoveryEntryV1.json() = obj("id" to id, "destination" to destination.json(), "text" to text,
    "image" to image?.let { obj("mime" to it.mime, "base64" to it.base64) }, "files" to files.map { it.json() },
    "status" to status, "payload" to payload?.json(), "start" to start?.json(), "created" to created?.json())
private fun entryV1(j: JSONObject) = RecoveryEntryV1(j.getLong("id"), destinationV1(j.getJSONObject("destination")),
    j.getString("text"), j.objectOrNull("image")?.let { RecoveryImageV1(it.getString("mime"), it.getString("base64")) },
    j.objects("files").map(::fileV1), j.getString("status"), j.objectOrNull("payload")?.let(::payloadV1),
    j.objectOrNull("start")?.let(::startV1), j.objectOrNull("created")?.let(::destinationV1))
