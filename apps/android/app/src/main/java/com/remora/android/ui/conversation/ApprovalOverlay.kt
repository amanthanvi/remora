package com.remora.android.ui.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.runtime.collectAsState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.material3.OutlinedTextField
import com.remora.android.ui.BerkeleyMono
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled
import com.remora.android.util.LLog
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AppStore
import uniffi.codex_mobile_client.ApprovalDecisionValue
import uniffi.codex_mobile_client.ApprovalKind
import uniffi.codex_mobile_client.PendingApproval
import uniffi.codex_mobile_client.PendingUserInputAnswer
import uniffi.codex_mobile_client.PendingUserInputRequest

/**
 * Full-screen overlay for pending approvals and user input requests.
 * Reads typed [PendingApproval] from Rust snapshot — no parsing needed.
 */
@Composable
fun ApprovalOverlay(
    approvals: List<PendingApproval>,
    userInputs: List<PendingUserInputRequest>,
    appStore: AppStore,
    onDismissUserInput: ((PendingUserInputRequest) -> Unit)? = null,
) {
    val scope = rememberCoroutineScope()
    var submittingRequestKey by remember { mutableStateOf<Triple<String, String?, String>?>(null) }
    var submitError by remember { mutableStateOf<String?>(null) }

    fun submitResponse(requestKey: Triple<String, String?, String>, kind: String, action: suspend () -> Unit) {
        scope.launch {
            submittingRequestKey = requestKey
            submitError = null
            try {
                action()
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                LLog.e(
                    TAG,
                    "$kind response failed",
                    error,
                    fields = mapOf("requestId" to requestKey.third),
                )
                submitError = responseSubmissionErrorMessage(error)
            } finally {
                if (submittingRequestKey == requestKey) {
                    submittingRequestKey = null
                }
            }
        }
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black.copy(alpha = 0.7f))
            .clickable(enabled = false) { /* block interaction */ },
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth(0.9f)
                .fillMaxHeight(0.85f)
                .padding(16.dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            submitError?.let { message ->
                Text(
                    text = message,
                    color = Color(0xFFFF6B6B),
                    fontSize = RemoraTextStyle.caption.scaled,
                )
            }

            for (approval in approvals) {
                val requestKey = Triple(approval.serverId, approval.runtimeKind, approval.id)
                key(requestKey) {
                    ApprovalCard(
                        approval = approval,
                        isSubmitting = submittingRequestKey == requestKey,
                        onDecision = { decision ->
                            submitResponse(requestKey, "approval") {
                                appStore.respondToApproval(approval.serverId, approval.runtimeKind, approval.id, decision)
                            }
                        },
                    )
                }
            }

            for (input in userInputs) {
                val requestKey = Triple(input.serverId, input.runtimeKind, input.id)
                key(requestKey) {
                    UserInputCard(
                        request = input,
                        isSubmitting = submittingRequestKey == requestKey,
                        onSubmit = { answers ->
                            submitResponse(requestKey, "user input") {
                                appStore.respondToUserInput(input.serverId, input.runtimeKind, input.id, answers)
                            }
                        },
                        onDismiss = { onDismissUserInput?.invoke(input) },
                    )
                }
            }
        }
    }
}

@Composable
private fun ApprovalCard(
    approval: PendingApproval,
    isSubmitting: Boolean,
    onDecision: (ApprovalDecisionValue) -> Unit,
) {
    val appModel = com.remora.android.ui.LocalAppModel.current
    val snap = appModel.snapshot.collectAsState()
    val context = androidx.compose.ui.platform.LocalContext.current
    val isLocal = snap.value?.servers?.firstOrNull { it.serverId == approval.serverId }?.isLocal == true
    val title = when (approval.kind) {
        ApprovalKind.COMMAND -> "Run command?"
        ApprovalKind.FILE_CHANGE -> "File change?"
        ApprovalKind.PERMISSIONS -> "Grant permission?"
        ApprovalKind.MCP_ELICITATION -> "Tool request"
    }

    // Bare layout (no card background) to match iOS ConversationView prompt.
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = title,
            color = RemoraTheme.textPrimary,
            fontSize = 16f.scaled,
        )

        // Command text — capped + scrollable so a long command can't push the
        // action buttons off-screen (issue #92).
        approval.command?.let { cmd ->
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(max = 220.dp)
                    .background(RemoraTheme.codeBackground, RoundedCornerShape(6.dp))
                    .verticalScroll(rememberScrollState())
                    .padding(8.dp),
            ) {
                Text(
                    text = cmd,
                    color = RemoraTheme.accent,
                    fontFamily = RemoraTheme.monoFont,
                    fontSize = RemoraTextStyle.code.scaled,
                )
            }
        }

        // CWD
        approval.cwd?.let { cwd ->
            Text(
                text = "in " + com.remora.android.state.PathDisplay.display(cwd, isLocal, context),
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.caption.scaled,
            )
        }

        // Path (for file changes)
        approval.path?.let { path ->
            Text(
                text = com.remora.android.state.PathDisplay.display(path, isLocal, context),
                color = RemoraTheme.textSecondary,
                fontFamily = RemoraTheme.monoFont,
                fontSize = RemoraTextStyle.caption.scaled,
            )
        }

        // Buttons
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            OutlinedButton(
                onClick = { onDecision(ApprovalDecisionValue.DECLINE) },
                enabled = !isSubmitting,
                modifier = Modifier.weight(1f),
            ) {
                Text("Deny")
            }
            OutlinedButton(
                onClick = { onDecision(ApprovalDecisionValue.ACCEPT_FOR_SESSION) },
                enabled = !isSubmitting,
                modifier = Modifier.weight(1f),
            ) {
                Text("Allow session")
            }
            Button(
                onClick = { onDecision(ApprovalDecisionValue.ACCEPT) },
                enabled = !isSubmitting,
                modifier = Modifier.weight(1f),
                colors = ButtonDefaults.buttonColors(
                    containerColor = RemoraTheme.accent,
                    contentColor = Color.Black,
                ),
            ) {
                Text("Allow")
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun UserInputCard(
    request: PendingUserInputRequest,
    isSubmitting: Boolean,
    onSubmit: (List<PendingUserInputAnswer>) -> Unit,
    onDismiss: (() -> Unit)? = null,
) {
    val answers = remember(request.serverId, request.runtimeKind, request.threadId, request.id) {
        mutableStateMapOf<String, String>()
    }

    // Bare layout (no card background) to match iOS ConversationView prompt.
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // Header with close button
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // Requester badge
            val requester = buildString {
                request.requesterAgentNickname?.let { append(it) }
                request.requesterAgentRole?.let {
                    if (isNotEmpty()) append(" ")
                    append("[$it]")
                }
            }
            if (requester.isNotBlank()) {
                Text(
                    text = requester,
                    color = RemoraTheme.accent,
                    fontSize = RemoraTextStyle.caption2.scaled,
                )
            } else {
                Spacer(modifier = Modifier.weight(1f))
            }
            if (onDismiss != null) {
                Text(
                    text = "✕",
                    color = RemoraTheme.textMuted,
                    fontSize = RemoraTextStyle.body.scaled,
                    modifier = Modifier
                        .clickable { onDismiss() }
                        .padding(4.dp)
                        .semantics { contentDescription = "Dismiss input request" },
                )
            }
        }

        for (question in request.questions) {
            key(question.id) {
                Text(
                    text = question.question,
                    color = RemoraTheme.textPrimary,
                    fontSize = RemoraTextStyle.body.scaled,
                )

                if (question.options.isNotEmpty()) {
                    // Keep long option labels from compressing their siblings.
                    FlowRow(
                        modifier = Modifier.selectableGroup(),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        for (option in question.options) {
                            val isSelected = answers[question.id] == option.label
                            Text(
                                text = option.label,
                                color = if (isSelected) Color.Black else RemoraTheme.textPrimary,
                                fontSize = RemoraTextStyle.caption.scaled,
                                fontWeight = if (isSelected) FontWeight.Bold else FontWeight.Normal,
                                modifier = Modifier
                                    .background(
                                        if (isSelected) RemoraTheme.accent else RemoraTheme.codeBackground,
                                        RoundedCornerShape(999.dp),
                                    )
                                    .selectable(
                                        selected = isSelected,
                                        enabled = !isSubmitting,
                                        role = Role.RadioButton,
                                        onClick = { answers[question.id] = option.label },
                                    )
                                    .heightIn(min = RemoraTheme.minimumTouchTarget)
                                    .wrapContentHeight()
                                    .padding(horizontal = 12.dp, vertical = 6.dp),
                            )
                        }
                    }
                } else {
                    OutlinedTextField(
                        value = answers[question.id].orEmpty(),
                        onValueChange = { answers[question.id] = it },
                        enabled = !isSubmitting,
                        label = { Text(question.header ?: "Answer") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        }

        Button(
            onClick = {
                val answerList = request.questions.map { q ->
                    PendingUserInputAnswer(
                        questionId = q.id,
                        answers = listOfNotNull(answers[q.id]),
                    )
                }
                onSubmit(answerList)
            },
            enabled = !isSubmitting,
            colors = ButtonDefaults.buttonColors(
                containerColor = RemoraTheme.accent,
                contentColor = Color.Black,
            ),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Submit")
        }
    }
}

private const val TAG = "ApprovalOverlay"
