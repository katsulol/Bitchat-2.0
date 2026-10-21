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
                    "writeFrame" -> {
                        val target = call.argument<String>("target")
                        val data = call.argument<ByteArray>("data")
                        if (data != null) {
                            bleService?.writeFrame(target, data)
                            result.success(true)
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
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            permissions.add(Manifest.permission.POST_NOTIFICATIONS)
        }
        return permissions
    }

    private fun hasAllPermissions(): Boolean {
        return getRequiredPermissions().all {
            ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED
        }
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
        val intent = Intent(this, BitchatBleService::class.java)
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                startForegroundService(intent)
            } else {
                startService(intent)
            }
            bindService(intent, serviceConnection, Context.BIND_AUTO_CREATE)

            window.decorView.postDelayed({
                bleService?.startMesh(shortIdHex)
            }, 300)
        } catch (e: Exception) {
            android.util.Log.e("MainActivity", "Failed to start BLE service", e)
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

    override fun onPeerConnected(address: String) {
        runOnUiThread {
            methodChannel?.invokeMethod("onPeerConnected", mapOf("address" to address))
        }
    }

    override fun onPeerDisconnected(address: String) {
        runOnUiThread {
            methodChannel?.invokeMethod("onPeerDisconnected", mapOf("address" to address))
        }
    }

    override fun onFrameReceived(address: String, data: ByteArray) {
        runOnUiThread {
            methodChannel?.invokeMethod("onFrameReceived", mapOf("address" to address, "data" to data))
        }
    }

    override fun onDestroy() {
        stopBleService()
        super.onDestroy()
    }
}
