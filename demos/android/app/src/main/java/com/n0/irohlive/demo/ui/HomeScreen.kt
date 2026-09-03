package com.n0.irohlive.demo.ui

import android.Manifest
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

/**
 * The three modes, and a way into the offline diagnostics.
 *
 * Everything else in the app hangs off one of these choices, so the screen
 * stays a single column with nothing to configure.
 */
@Composable
fun HomeScreen(
    onWatch: () -> Unit,
    onPublish: () -> Unit,
    onCall: () -> Unit,
    onLoopback: (encoded: Boolean) -> Unit,
    onDenied: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val withCamera = rememberPermissionGate(listOf(Manifest.permission.CAMERA), onDenied)

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 24.dp, vertical = 32.dp),
    ) {
        Text("iroh-live", style = MaterialTheme.typography.displaySmall)
        Spacer(Modifier.height(6.dp))
        Text(
            "Video between two devices, straight over iroh.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(32.dp))

        ModeCard(
            title = "Watch",
            detail = "Scan or paste a ticket and play it full screen.",
            container = MaterialTheme.colorScheme.primaryContainer,
            onContainer = MaterialTheme.colorScheme.onPrimaryContainer,
            onClick = onWatch,
            modifier = Modifier.weight(1f),
        )

        Spacer(Modifier.height(16.dp))

        ModeCard(
            title = "Publish",
            detail = "Send this camera and microphone, and show the ticket as a QR code.",
            container = MaterialTheme.colorScheme.surfaceVariant,
            onContainer = MaterialTheme.colorScheme.onSurface,
            onClick = onPublish,
            modifier = Modifier.weight(1f),
        )

        Spacer(Modifier.height(16.dp))

        ModeCard(
            title = "Call",
            detail = "Both ends send and receive. Show a code to be called, or scan one to call.",
            container = MaterialTheme.colorScheme.secondaryContainer,
            onContainer = MaterialTheme.colorScheme.onSecondaryContainer,
            onClick = onCall,
            modifier = Modifier.weight(1f),
        )

        Spacer(Modifier.height(24.dp))

        Text(
            "Diagnostics",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = { withCamera { onLoopback(false) } }) {
                Text("Camera passthrough")
            }
            TextButton(onClick = { withCamera { onLoopback(true) } }) {
                Text("H.264 loopback")
            }
        }
    }
}

@Composable
private fun ModeCard(
    title: String,
    detail: String,
    container: Color,
    onContainer: Color,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(
        onClick = onClick,
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.cardColors(containerColor = container, contentColor = onContainer),
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(24.dp),
            verticalArrangement = Arrangement.Bottom,
            horizontalAlignment = Alignment.Start,
        ) {
            Text(title, style = MaterialTheme.typography.headlineLarge)
            Spacer(Modifier.height(8.dp))
            Text(detail, style = MaterialTheme.typography.bodyMedium)
        }
    }
}
