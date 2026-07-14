package com.remora.android.ui.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled

@Composable
internal fun ConversationCodeBlock(
    language: String?,
    code: String,
    modifier: Modifier = Modifier,
    bodySize: Float = RemoraTextStyle.body,
) {
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        language?.takeIf { it.isNotBlank() }?.let {
            Text(
                text = it.uppercase(),
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.caption2.scaled,
                fontWeight = FontWeight.Bold,
            )
        }
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.codeBackground, RoundedCornerShape(8.dp))
                .padding(10.dp),
        ) {
            if (isDiffLanguage(language)) {
                SyntaxHighlightedDiffBlock(
                    diff = code,
                    titleHint = language,
                    fontSize = RemoraTextStyle.caption.sp,
                    modifier = Modifier.fillMaxWidth(),
                )
            } else {
                SelectableConversationText {
                    Text(
                        text = code,
                        color = RemoraTheme.textBody,
                        fontFamily = RemoraTheme.monoFont,
                        fontSize = bodySize.scaled,
                        modifier = Modifier.horizontalScroll(rememberScrollState()),
                    )
                }
            }
        }
    }
}
