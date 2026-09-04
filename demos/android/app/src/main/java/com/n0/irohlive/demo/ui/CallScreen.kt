package com.n0.irohlive.demo.ui

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import com.n0.irohlive.demo.RenderTarget

/**
 * The two ways into a call: scan somebody's code, or show one and be scanned.
 *
 * Both ends of a call publish and subscribe, so which one dialed stops
 * mattering the moment the session is up. The choice here is only about who
 * has the other's address.
 */
@Composable
fun CallSetupScreen(
    ticket: String,
    busy: Boolean,
    onTicketChange: (String) -> Unit,
    onCall: (String) -> Unit,
    onWait: () -> Unit,
    onBack: () -> Unit,
    onDenied: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scanner = rememberLauncherForActivityResult(ScanContract()) { result ->
        val contents = result.contents
        if (contents != null) {
            onTicketChange(contents)
            onCall(contents)
        }
    }
    val withCall = rememberPermissionGate(
        listOf(Manifest.permission.CAMERA, Manifest.permission.RECORD_AUDIO),
        onDenied,
    )

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
        Text("Call", style = MaterialTheme.typography.displaySmall)
        Spacer(Modifier.height(6.dp))
        Text(
            "Both ends send their camera and microphone and play the other's.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(28.dp))

        Button(
            onClick = {
                withCall {
                    scanner.launch(
                        ScanOptions().apply {
                            setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                            setPrompt("Scan the other device's call code")
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
            Text("Scan a code and call", style = MaterialTheme.typography.titleMedium)
        }

        Spacer(Modifier.height(16.dp))

        OutlinedButton(
            onClick = { withCall { onWait() } },
            enabled = !busy,
            modifier = Modifier
                .fillMaxWidth()
                .height(64.dp),
        ) {
            Text("Show my code and wait", style = MaterialTheme.typography.titleMedium)
        }

        Spacer(Modifier.height(28.dp))

        Text(
            "Or paste a call ticket",
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
            onClick = { withCall { onCall(ticket) } },
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
                Text("Call")
            }
        }
    }
}

/**
 * A call, in both of its states.
 *
 * While `ticket` is set nobody has dialed yet, so the screen is the code over
 * this node's own camera, which is also what the renderer is drawing. When a
 * peer arrives the code goes and the same surface carries their picture, so the
 * transition is one overlay disappearing rather than a screen change.
 */
@Composable
fun CallScreen(
    ticket: String?,
    status: String,
    target: RenderTarget,
    onCopy: (String) -> Unit,
    onStop: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Box(modifier = modifier.fillMaxSize().background(Color.Black)) {
        VideoSurface(target = target, modifier = Modifier.fillMaxSize())

        if (ticket != null) {
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
                    "Waiting for a call",
                    style = MaterialTheme.typography.headlineSmall,
                    color = Color.White,
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    "Scan this from the other device.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = Color.White.copy(alpha = 0.7f),
                )
                Spacer(Modifier.height(24.dp))
                TicketCard(ticket = ticket, onCopy = onCopy)
            }
        }

        Column(
            modifier = Modifier
                .fillMaxWidth()
                .safeDrawingPadding()
                .padding(16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            if (status.isNotEmpty()) {
                Text(
                    status,
                    style = MaterialTheme.typography.labelSmall,
                    color = Color.White.copy(alpha = 0.7f),
                )
            }
        }

        TextButton(
            onClick = onStop,
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .safeDrawingPadding()
                .padding(24.dp),
        ) {
            Text("End call", color = Color.White)
        }
    }
}
