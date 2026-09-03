package com.n0.irohlive.demo.ui

import android.Manifest
import androidx.camera.view.PreviewView
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView

/** Names the broadcast, then goes live. */
@Composable
fun PublishSetupScreen(
    name: String,
    busy: Boolean,
    onNameChange: (String) -> Unit,
    onPublish: (String) -> Unit,
    onBack: () -> Unit,
    onDenied: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val withCaptureAccess = rememberPermissionGate(
        listOf(Manifest.permission.CAMERA, Manifest.permission.RECORD_AUDIO),
        onDenied,
    )

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 24.dp, vertical = 24.dp),
    ) {
        TextButton(onClick = onBack, contentPadding = PaddingValues(0.dp)) { Text("Back") }
        Spacer(Modifier.height(12.dp))
        Text("Publish", style = MaterialTheme.typography.displaySmall)
        Spacer(Modifier.height(6.dp))
        Text(
            "The camera and microphone go out under this name. Anyone with the " +
                "ticket can watch.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(28.dp))

        OutlinedTextField(
            value = name,
            onValueChange = onNameChange,
            label = { Text("Broadcast name") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(16.dp))

        Button(
            onClick = { withCaptureAccess { onPublish(name) } },
            enabled = !busy,
            modifier = Modifier
                .fillMaxWidth()
                .height(64.dp),
        ) {
            Text("Go live", style = MaterialTheme.typography.titleMedium)
        }
    }
}

/**
 * The live broadcast: its ticket as a QR code, over the camera it is sending.
 *
 * The preview sits behind a scrim rather than in a thumbnail so that the one
 * thing a second device needs, the QR code, stays as large as the screen allows.
 */
@Composable
fun PublishLiveScreen(
    name: String,
    ticket: String?,
    onCopy: (String) -> Unit,
    onStop: () -> Unit,
    onPreviewView: (PreviewView?) -> Unit,
    modifier: Modifier = Modifier,
) {
    Box(modifier = modifier.fillMaxSize().background(Color.Black)) {
        CameraPreview(onPreviewView = onPreviewView, modifier = Modifier.fillMaxSize())
        Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.72f)))

        Column(
            modifier = Modifier
                .fillMaxSize()
                .safeDrawingPadding()
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Text(
                "Live as \"$name\"",
                style = MaterialTheme.typography.titleLarge,
                color = Color.White,
            )
            Spacer(Modifier.height(20.dp))

            if (ticket == null) {
                CircularProgressIndicator()
                Spacer(Modifier.height(16.dp))
                Text(
                    "Starting the broadcast",
                    style = MaterialTheme.typography.bodyMedium,
                    color = Color.White.copy(alpha = 0.7f),
                )
            } else {
                TicketCard(ticket = ticket, onCopy = onCopy)
            }

            Spacer(Modifier.height(28.dp))

            OutlinedButton(
                onClick = onStop,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = Color.White),
                modifier = Modifier.fillMaxWidth().height(52.dp),
            ) {
                Text("Stop broadcasting")
            }
        }
    }
}

@Composable
private fun TicketCard(ticket: String, onCopy: (String) -> Unit) {
    val qr = remember(ticket) { qrBitmap(ticket) }
    Card(
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.cardColors(containerColor = Color.White),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(20.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            if (qr != null) {
                Image(
                    bitmap = qr,
                    contentDescription = "Broadcast ticket",
                    // Nearest-neighbour keeps the module edges square when the
                    // bitmap is scaled up to the card width.
                    filterQuality = FilterQuality.None,
                    contentScale = ContentScale.Fit,
                    modifier = Modifier.fillMaxWidth().aspectRatio(1f),
                )
            } else {
                Text(
                    "This ticket is too long for a QR code.",
                    color = Color.Black,
                    textAlign = TextAlign.Center,
                )
            }
            Spacer(Modifier.height(12.dp))
            Text(
                text = ticket,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                color = Color(0xFF444444),
                textAlign = TextAlign.Center,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.height(8.dp))
            TextButton(onClick = { onCopy(ticket) }) { Text("Copy ticket") }
        }
    }
}

@Composable
private fun CameraPreview(onPreviewView: (PreviewView?) -> Unit, modifier: Modifier = Modifier) {
    AndroidView(
        modifier = modifier,
        factory = { context ->
            PreviewView(context).apply {
                scaleType = PreviewView.ScaleType.FILL_CENTER
                onPreviewView(this)
            }
        },
        onRelease = { onPreviewView(null) },
    )
}
