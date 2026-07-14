package com.remora.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Neutral Remora splash surface using the shared black, green, and mono visual system. */
@Composable
fun AnimatedSplashScreen() {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(RemoraTheme.background),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            AnimatedLogo(size = 152.dp)
            Text(
                text = "REMORA",
                color = RemoraTheme.textPrimary,
                fontFamily = RemoraTheme.monoFont,
                fontSize = 24.sp,
                fontWeight = FontWeight.SemiBold,
                letterSpacing = 5.sp,
            )
            Text(
                text = "Your coding workspace, wherever you are.",
                color = RemoraTheme.textMuted,
                fontFamily = RemoraTheme.monoFont,
                fontSize = 13.sp,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(horizontal = 32.dp),
            )
        }
    }
}
