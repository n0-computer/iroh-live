package com.n0.irohlive.demo.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

/**
 * The one colour scheme the demo uses.
 *
 * Watch mode fills the screen with video, so the rest of the app is dark too:
 * a light home screen would flare every time you leave the player.
 */
private val DemoColors = darkColorScheme(
    primary = Color(0xFF9E86FF),
    onPrimary = Color(0xFF16112B),
    primaryContainer = Color(0xFF362A66),
    onPrimaryContainer = Color(0xFFE4DBFF),
    secondary = Color(0xFF7FD4C1),
    onSecondary = Color(0xFF06231D),
    background = Color(0xFF101014),
    onBackground = Color(0xFFE6E4EC),
    surface = Color(0xFF101014),
    onSurface = Color(0xFFE6E4EC),
    surfaceVariant = Color(0xFF232430),
    onSurfaceVariant = Color(0xFFB9B7C6),
    outline = Color(0xFF4A4B5A),
    error = Color(0xFFFFB4A9),
    onError = Color(0xFF3B0906),
)

/** Wraps [content] in the demo's Material 3 theme. */
@Composable
fun IrohLiveTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = DemoColors, content = content)
}
