package com.n0.irohlive.demo.ui

import android.Manifest
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.clickable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import androidx.activity.compose.rememberLauncherForActivityResult
import com.n0.irohlive.demo.RenderTarget
import kotlinx.coroutines.delay

/** How long the player's controls stay up after a tap. */
private const val CHROME_TIMEOUT_MILLIS = 3500L

/**
 * Ticket entry, with the QR scanner as the primary route.
 *
 * Typing a ticket by hand is possible and painful, so the scan button comes
 * first and the text field is the fallback.
 */
@Composable
fun WatchSetupScreen(
    ticket: String,
    busy: Boolean,
    onTicketChange: (String) -> Unit,
    onWatch: (String) -> Unit,
    onBack: () -> Unit,
    onDenied: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scanner = rememberLauncherForActivityResult(ScanContract()) { result ->
        val contents = result.contents
        if (contents != null) {
            onTicketChange(contents)
            onWatch(contents)
        }
    }
    val withCamera = rememberPermissionGate(listOf(Manifest.permission.CAMERA), onDenied)

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 24.dp),
    ) {
        TextButton(onClick = onBack, contentPadding = PaddingValues(0.dp)) {
            Text("Back")
        }
        Spacer(Modifier.height(12.dp))
        Text("Watch", style = MaterialTheme.typography.displaySmall)
        Spacer(Modifier.height(6.dp))
        Text(
            "Point the camera at the QR code on the publishing device.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(28.dp))

        Button(
            onClick = {
                withCamera {
                    scanner.launch(
                        ScanOptions().apply {
                            setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                            setPrompt("Scan an iroh-live ticket")
                            setBeepEnabled(false)
                            setOrientationLocked(false)
                        }
                    )
                }
            },
            enabled = !busy,
            modifier = Modifier
                .fillMaxWidth()
                .height(64.dp),
        ) {
            Text("Scan QR code", style = MaterialTheme.typography.titleMedium)
        }

        Spacer(Modifier.height(28.dp))

        Text(
            "Or paste a ticket",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = ticket,
            onValueChange = onTicketChange,
            placeholder = { Text("iroh-live ticket") },
            minLines = 3,
            maxLines = 6,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(16.dp))

        Button(
            onClick = { onWatch(ticket) },
            enabled = !busy && ticket.isNotBlank(),
            modifier = Modifier
                .fillMaxWidth()
                .height(56.dp),
        ) {
            if (busy) {
                CircularProgressIndicator(
                    modifier = Modifier.height(20.dp),
                    strokeWidth = 2.dp,
                    color = MaterialTheme.colorScheme.onPrimary,
                )
            } else {
                Text("Watch")
            }
        }
    }
}

/**
 * The player: video edge to edge, and nothing else once the controls time out.
 *
 * Tapping brings back a close button, the status line, and the rendition
 * picker when the broadcast offers more than one.
 */
@Composable
fun WatchPlayer(
    title: String,
    status: String,
    renditions: List<String>,
    target: RenderTarget,
    onSelectRendition: (String) -> Unit,
    onExit: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var chromeVisible by remember { mutableStateOf(true) }
    var selected by remember { mutableStateOf<String?>(null) }
    val interaction = remember { MutableInteractionSource() }

    LaunchedEffect(chromeVisible) {
        if (chromeVisible) {
            delay(CHROME_TIMEOUT_MILLIS)
            chromeVisible = false
        }
    }

    Box(
        modifier = modifier
            .fillMaxSize()
            .background(Color.Black)
            .clickable(interactionSource = interaction, indication = null) {
                chromeVisible = !chromeVisible
            },
    ) {
        VideoSurface(target = target, modifier = Modifier.fillMaxSize())

        AnimatedVisibility(visible = chromeVisible, enter = fadeIn(), exit = fadeOut()) {
            Box(Modifier.fillMaxSize().safeDrawingPadding()) {
                IconButton(
                    onClick = onExit,
                    colors = IconButtonDefaults.iconButtonColors(
                        containerColor = Color.Black.copy(alpha = 0.45f),
                        contentColor = Color.White,
                    ),
                    modifier = Modifier
                        .align(Alignment.TopStart)
                        .padding(12.dp),
                ) {
                    Icon(Icons.Default.Close, contentDescription = "Leave")
                }

                Column(
                    modifier = Modifier
                        .align(Alignment.BottomStart)
                        .fillMaxWidth()
                        .padding(16.dp),
                ) {
                    if (renditions.size > 1) {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            renditions.forEach { name ->
                                FilterChip(
                                    selected = selected == name,
                                    onClick = {
                                        selected = name
                                        onSelectRendition(name)
                                        chromeVisible = true
                                    },
                                    label = { Text(name) },
                                )
                            }
                        }
                        Spacer(Modifier.height(8.dp))
                    }
                    Text(
                        text = if (status.isEmpty()) title else status,
                        style = MaterialTheme.typography.bodySmall,
                        color = Color.White.copy(alpha = 0.8f),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}
