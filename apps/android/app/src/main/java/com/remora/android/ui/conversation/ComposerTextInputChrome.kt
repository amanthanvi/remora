package com.remora.android.ui.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.OpenInFull
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled

@Composable
internal fun ComposerTextInputChrome(
    value: TextFieldValue,
    onValueChange: (TextFieldValue) -> Unit,
    showExpand: Boolean,
    onExpand: () -> Unit,
    modifier: Modifier = Modifier,
    textFieldModifier: Modifier = Modifier,
    overlays: @Composable BoxScope.() -> Unit = {},
    trailingContent: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier = modifier
            .heightIn(min = 36.dp, max = 120.dp)
            .background(RemoraTheme.codeBackground, RoundedCornerShape(18.dp))
            .padding(horizontal = 14.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(modifier = Modifier.weight(1f)) {
            if (value.text.isEmpty()) {
                Text(
                    text = "Message\u2026",
                    color = RemoraTheme.textMuted,
                    fontSize = RemoraTextStyle.body.scaled,
                )
            }
            BasicTextField(
                value = value,
                onValueChange = onValueChange,
                textStyle = TextStyle(
                    color = RemoraTheme.textPrimary,
                    fontSize = RemoraTextStyle.body.scaled,
                    fontFamily = RemoraTheme.monoFont,
                ),
                cursorBrush = SolidColor(RemoraTheme.accent),
                // Reserve trailing space even while the expand affordance is hidden,
                // so wrapped lines and the field width do not jump as it appears.
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(end = 24.dp)
                    .then(textFieldModifier),
            )

            if (showExpand) {
                IconButton(
                    onClick = onExpand,
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .size(20.dp),
                ) {
                    Icon(
                        imageVector = Icons.Default.OpenInFull,
                        contentDescription = "Expand composer",
                        tint = RemoraTheme.textSecondary,
                        modifier = Modifier.size(12.dp),
                    )
                }
            }

            overlays()
        }
        trailingContent()
    }
}
