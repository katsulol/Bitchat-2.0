package com.bitchat.bitchat2

import android.Manifest
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.Build
import android.os.IBinder
import androidx.annotation.NonNull
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity(), BitchatBleService.BleEventListener {

    companion object {
        const val BLE_CHANNEL = "com.bitchat.mesh/ble"
        private const val PERMISSION_REQUEST_CODE = 1001
    }

    private var methodChannel: MethodChannel? = null
    private var bleService: BitchatBleService? = null
    private var isBound = false
    private var pendingShortIdHex: String? = null

    private val serviceConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, service: IBinder?) {
            val binder = service as BitchatBleService.LocalBinder
            bleService = binder.getService()
            bleService?.listener = this@MainActivity
            isBound = true
            pendingShortIdHex?.let {
                bleService?.startMesh(it)
                pendingShortIdHex = null
            }
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            bleService?.listener = null
            bleService = null
            isBound = false
        }
    }

    override fun configureFlutterEngine(@NonNull flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)

        methodChannel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, BLE_CHANNEL).apply {
            setMethodCallHandler { call, result ->
                when (call.method) {
                    "startBle" -> {
                        val shortId = call.argument<String>("shortId") ?: ""
                        checkPermissionsAndStart(shortId)
                        result.success(true)
                    }
                    "stopBle" -> {
                        stopBleService()
                        result.success(true)
                    }
                    "getAppStoragePath" -> {
                        result.success(applicationContext.filesDir.absolutePath)
                    }
                    "writeFrame" -> {
                        val target = call.argument<String>("target")
                        val data = call.argument<ByteArray>("data")
                        if (data != null) {
                            val svc = bleService
                            if (svc != null) {
                                svc.writeFrame(target, data)
                                result.success(true)
                            } else {
                                result.error("SERVICE_NOT_BOUND", "BitchatBleService is not bound", null)
                            }
                        } else {
                            result.error("INVALID_ARG", "Data buffer cannot be null", null)
                        }
                    }
                    else -> result.notImplemented()
                }
            }
        }
    }

    private fun getRequiredPermissions(): List<String> {
        val permissions = mutableListOf<String>()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            permissions.add(Manifest.permission.BLUETOOTH_SCAN)
            permissions.add(Manifest.permission.BLUETOOTH_ADVERTISE)
            permissions.add(Manifest.permission.BLUETOOTH_CONNECT)
        }
        permissions.add(Manifest.permission.ACCESS_FINE_LOCATION)
        permissions.add(Manifest.permission.ACCESS_COARSE_LOCATION)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            permissions.add(Manifest.permission.POST_NOTIFICATIONS)
        }
        return permissions
    }

    private fun hasAllPermissions(): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val scan = ContextCompat.checkSelfPermission(this, Manifest.permission.BLUETOOTH_SCAN) == PackageManager.PERMISSION_GRANTED
            val connect = ContextCompat.checkSelfPermission(this, Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
            val advertise = ContextCompat.checkSelfPermission(this, Manifest.permission.BLUETOOTH_ADVERTISE) == PackageManager.PERMISSION_GRANTED
            if (!scan || !connect || !advertise) return false
        } else {
            val fine = ContextCompat.checkSelfPermission(this, Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
            val coarse = ContextCompat.checkSelfPermission(this, Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED
            if (!fine && !coarse) return false
        }
        return true
    }

    private fun checkPermissionsAndStart(shortIdHex: String) {
        pendingShortIdHex = shortIdHex
        if (hasAllPermissions()) {
            startBleService(shortIdHex)
        } else {
            val missing = getRequiredPermissions().filter {
                ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
            }.toTypedArray()
            ActivityCompat.requestPermissions(this, missing, PERMISSION_REQUEST_CODE)
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == PERMISSION_REQUEST_CODE) {
            if (hasAllPermissions()) {
                pendingShortIdHex?.let { startBleService(it) }
            }
        }
    }

    private fun startBleService(shortIdHex: String) {
        if (isBound && bleService != null) {
            bleService?.startMesh(shortIdHex)
            return
        }

        val intent = Intent(this, BitchatBleService::class.java)
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                try {
                    startForegroundService(intent)
                } catch (fgsEx: Exception) {
                    android.util.Log.w("MainActivity", "startForegroundService deferred: ${fgsEx.message}")
                    startService(intent)
                }
            } else {
                startService(intent)
            }
        } catch (e: Exception) {
            android.util.Log.w("MainActivity", "Fallback startService failed: ${e.message}")
        }

        try {
            bindService(intent, serviceConnection, Context.BIND_AUTO_CREATE)
        } catch (e: Exception) {
            android.util.Log.e("MainActivity", "Failed to bind BLE service", e)
        }
    }

    private fun stopBleService() {
        if (isBound) {
            unbindService(serviceConnection)
            isBound = false
        }
        val intent = Intent(this, BitchatBleService::class.java)
        stopService(intent)
    }

    override fun onPeerConnected(shortId: String) {
        runOnUiThread {
            methodChannel?.invokeMethod("onPeerConnected", mapOf("shortId" to shortId))
        }
    }

    override fun onPeerDisconnected(shortId: String) {
        runOnUiThread {
            methodChannel?.invokeMethod("onPeerDisconnected", mapOf("shortId" to shortId))
        }
    }

    override fun onFrameReceived(senderShortId: String, data: ByteArray) {
        runOnUiThread {
            methodChannel?.invokeMethod("onFrameReceived", mapOf("sender" to senderShortId, "data" to data))
        }
    }

    override fun onDestroy() {
        stopBleService()
        super.onDestroy()
    }
}
