package com.remora.android.ui.home

import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled

/**
 * Glass-ish capsule that morphs into a focused search field when tapped.
 * Caller controls expanded state and reads the query via callbacks so it
 * can render the results list alongside.
 */
@Composable
fun ThreadSearchBar(
    query: String,
    isExpanded: Boolean,
    onQueryChange: (String) -> Unit,
    onExpandChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    focusRequest: Int = 0,
    onFocusChanged: (Boolean) -> Unit = {},
) {
    val focusRequester = remember { FocusRequester() }

    LaunchedEffect(isExpanded, focusRequest) {
        if (isExpanded) {
            focusRequester.requestFocus()
        }
    }
    DisposableEffect(Unit) {
        onDispose { onFocusChanged(false) }
    }

    Row(
        modifier = modifier.fillMaxWidth(),
        horizontalArrangement = if (isExpanded) Arrangement.Start else Arrangement.End,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val rowModifier = if (isExpanded) {
            Modifier.fillMaxWidth()
        } else {
            Modifier.wrapContentWidth()
        }

        Row(
            modifier = rowModifier
                .animateContentSize(
                    animationSpec = spring(
                        dampingRatio = Spring.DampingRatioLowBouncy,
                        stiffness = Spring.StiffnessMediumLow,
                    ),
                )
                .background(
                    color = RemoraTheme.textPrimary.copy(alpha = 0.06f),
                    shape = CircleShape,
                )
                .border(
                    width = if (isExpanded) 1.dp else 0.5.dp,
                    color = if (isExpanded) RemoraTheme.accent.copy(alpha = 0.5f)
                    else RemoraTheme.textMuted.copy(alpha = 0.25f),
                    shape = CircleShape,
                )
                .heightIn(min = RemoraTheme.minimumTouchTarget)
                .clickable(enabled = !isExpanded) { onExpandChange(true) }
                .padding(
                    horizontal = if (isExpanded) 12.dp else 12.dp,
                    vertical = if (isExpanded) 6.dp else 6.dp,
                ),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                Icons.Default.Search,
                contentDescription = null,
                tint = if (isExpanded) RemoraTheme.accent else RemoraTheme.textSecondary,
                modifier = Modifier.size(14.dp),
            )
            Box(modifier = Modifier.width(6.dp))

            if (isExpanded) {
                BasicTextField(
                    value = query,
                    onValueChange = onQueryChange,
                    modifier = Modifier
                        .weight(1f)
                        .focusRequester(focusRequester)
                        .onFocusChanged { onFocusChanged(it.isFocused) },
                    textStyle = TextStyle(
                        color = RemoraTheme.textPrimary,
                        fontSize = RemoraTextStyle.code.scaled,
                        fontFamily = FontFamily.Monospace,
                    ),
                    singleLine = true,
                    cursorBrush = SolidColor(RemoraTheme.accent),
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                    decorationBox = { inner ->
                        Box {
                            if (query.isEmpty()) {
                                Text(
                                    text = "search threads",
                                    color = RemoraTheme.textSecondary,
                                    fontSize = RemoraTextStyle.code.scaled,
                                    fontFamily = FontFamily.Monospace,
                                )
                            }
                            inner()
                        }
                    },
                )
                IconButton(
                    onClick = {
                        onQueryChange("")
                        onExpandChange(false)
                    },
                    modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
                ) {
                    Icon(
                        Icons.Default.Close,
                        contentDescription = "Close",
                        tint = RemoraTheme.textMuted,
                        modifier = Modifier.size(14.dp),
                    )
                }
            } else {
                Text(
                    text = "search threads",
                    color = RemoraTheme.textSecondary,
                    fontSize = RemoraTextStyle.caption.scaled,
                    fontFamily = FontFamily.Monospace,
                    textAlign = TextAlign.Start,
                )
            }
        }
    }
}
