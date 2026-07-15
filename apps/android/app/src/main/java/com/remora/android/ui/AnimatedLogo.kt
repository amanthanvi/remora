package com.remora.android.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.remora.android.R

/** Compact Remora mark for splash and navigation surfaces. */
@Composable
fun AnimatedLogo(size: Dp = 44.dp) {
    Image(
        painter = painterResource(R.drawable.remora_mascot),
        contentDescription = null,
        contentScale = ContentScale.Fit,
        modifier = Modifier.size(size),
    )
}
