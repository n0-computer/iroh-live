package com.n0.irohlive.demo.ui

import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat

/**
 * Returns a function that runs an action once [permissions] are all granted.
 *
 * Permissions are asked for at the point of use rather than at launch: watch
 * mode needs the camera only to scan a QR code, and someone who only ever
 * watches should never see a microphone prompt.
 */
@Composable
fun rememberPermissionGate(
    permissions: List<String>,
    onDenied: () -> Unit,
): ((() -> Unit) -> Unit) {
    val context = LocalContext.current
    var pending by remember { mutableStateOf<(() -> Unit)?>(null) }
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { grants ->
        val action = pending
        pending = null
        if (grants.values.all { it }) action?.invoke() else onDenied()
    }
    return { action ->
        val missing = permissions.filter {
            ContextCompat.checkSelfPermission(context, it) != PackageManager.PERMISSION_GRANTED
        }
        if (missing.isEmpty()) {
            action()
        } else {
            pending = action
            launcher.launch(missing.toTypedArray())
        }
    }
}
