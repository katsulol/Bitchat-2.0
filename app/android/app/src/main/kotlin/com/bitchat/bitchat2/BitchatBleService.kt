package com.bitchat.bitchat2

import android.Manifest
import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.bluetooth.*
import android.bluetooth.le.*
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.*
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.util.*
import java.util.concurrent.ConcurrentHashMap

/**
 * Robust Dual-role BLE Mesh Foreground Service.
 *
 * Runs simultaneously as:
 * 1. BLE Peripheral (Advertising service 0xFD6F, hosting GATT server)
 * 2. BLE Central (Scanning for peer advertisements, connecting, subscribing to notifications)
 * 3. Foreground Service maintaining radio active state across Android lifecycle.
 */
class BitchatBleService : Service() {

    companion object {
        const val TAG = "BitchatBleService"
        const val CHANNEL_ID = "bitchat_ble_mesh"
        const val NOTIFICATION_ID = 2001

        val SERVICE_UUID: UUID = UUID.fromString("0000FD6F-0000-1000-8000-00805F9B34FB")
        val CHAR_WRITE_UUID: UUID = UUID.fromString("0000FD60-0000-1000-8000-00805F9B34FB")
        val CHAR_NOTIFY_UUID: UUID = UUID.fromString("0000FD61-0000-1000-8000-00805F9B34FB")
        val CCCD_UUID: UUID = UUID.fromString("00002902-0000-1000-8000-00805F9B34FB")
    }

    private val binder = LocalBinder()
    private val mainHandler = Handler(Looper.getMainLooper())
    private var bluetoothManager: BluetoothManager? = null
    private var bluetoothAdapter: BluetoothAdapter? = null

    // Peripheral
    private var advertiser: BluetoothLeAdvertiser? = null
    private var gattServer: BluetoothGattServer? = null
    private val serverConnectedDevices = ConcurrentHashMap.newKeySet<BluetoothDevice>()

    // Central
    private var scanner: BluetoothLeScanner? = null
    private val outgoingGatts = ConcurrentHashMap<String, BluetoothGatt>()
    private val connectingDevices = ConcurrentHashMap.newKeySet<String>()

    // Current Short ID
    private var localShortIdHex: String = ""
    private var isScanning = false

    // Callback listener for Flutter integration
    var listener: BleEventListener? = null

    interface BleEventListener {
        fun onPeerConnected(address: String)
        fun onPeerDisconnected(address: String)
        fun onFrameReceived(address: String, data: ByteArray)
    }

    inner class LocalBinder : Binder() {
        fun getService(): BitchatBleService = this@BitchatBleService
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        if (hasRequiredPermissions()) {
            startServiceInForeground()
        }

        bluetoothManager = getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        bluetoothAdapter = bluetoothManager?.adapter

        try {
            val filter = android.content.IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED)
            registerReceiver(bluetoothReceiver, filter)
        } catch (_: Exception) {}
    }

    private val bluetoothReceiver = object : android.content.BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: android.content.Intent?) {
            if (intent?.action == BluetoothAdapter.ACTION_STATE_CHANGED) {
                val state = intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, BluetoothAdapter.ERROR)
                if (state == BluetoothAdapter.STATE_ON) {
                    Log.i(TAG, "Bluetooth switched to STATE_ON. Initializing mesh...")
                    bluetoothManager = getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
                    bluetoothAdapter = bluetoothManager?.adapter
                    mainHandler.postDelayed({
                        startMesh(localShortIdHex)
                    }, 500)
                }
            }
        }
    }

    fun hasRequiredPermissions(): Boolean {
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

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Bitchat Mesh Service",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "Maintains Bluetooth Low Energy mesh routing in background"
            }
            val manager = getSystemService(NotificationManager::class.java)
            manager?.createNotificationChannel(channel)
        }
    }

    private fun startServiceInForeground() {
        val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Bitchat 2.0 Mesh Active")
            .setContentText("Listening and forwarding peer frames via BLE")
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()

        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(
                    NOTIFICATION_ID,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
                )
            } else {
                startForeground(NOTIFICATION_ID, notification)
            }
        } catch (e: Throwable) {
            Log.w(TAG, "Foreground service start deferred: ${e.message}")
        }
    }

    @SuppressLint("MissingPermission")
    fun startMesh(shortIdHex: String) {
        if (!hasRequiredPermissions()) {
            Log.w(TAG, "Cannot start mesh: missing Bluetooth permissions")
            return
        }
        startServiceInForeground()
        localShortIdHex = shortIdHex

        val adapter = bluetoothAdapter ?: return
        if (!adapter.isEnabled) {
            Log.w(TAG, "Bluetooth adapter is disabled")
            return
        }

        // Clean any existing state
        stopMeshInternal()

        mainHandler.postDelayed({
            startGattServer()
            startAdvertising()
            startScanning()
            startPeriodicScanRefresh()
        }, 300)
    }

    @SuppressLint("MissingPermission")
    fun stopMesh() {
        stopPeriodicScanRefresh()
        stopMeshInternal()
    }

    @SuppressLint("MissingPermission")
    private fun stopMeshInternal() {
        stopAdvertising()
        stopScanning()
        closeGattServer()
        disconnectAllOutgoing()
    }

    @SuppressLint("MissingPermission")
    private fun disconnectAllOutgoing() {
        for ((_, gatt) in outgoingGatts) {
            try {
                gatt.disconnect()
                gatt.close()
            } catch (_: Exception) {}
        }
        outgoingGatts.clear()
        connectingDevices.clear()
        serverConnectedDevices.clear()
    }

    @SuppressLint("MissingPermission")
    private fun startAdvertising() {
        bluetoothManager = getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        bluetoothAdapter = bluetoothManager?.adapter
        advertiser = bluetoothAdapter?.bluetoothLeAdvertiser
        if (advertiser == null) {
            Log.e(TAG, "BLE advertiser not supported on this device")
            return
        }

        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_HIGH)
            .setConnectable(true)
            .setTimeout(0)
            .build()

        val pUuid = ParcelUuid(SERVICE_UUID)
        val shortBytes = hexStringToByteArray(localShortIdHex.ifEmpty { "0000000000000000" })

        // Primary 31-byte packet: Service UUID only (<21 bytes)
        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .addServiceUuid(pUuid)
            .build()

        // Scan response packet: Service UUID + ShortId service data (<22 bytes)
        val scanResponse = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .addServiceUuid(pUuid)
            .addServiceData(pUuid, shortBytes)
            .build()

        try {
            advertiser?.startAdvertising(settings, data, scanResponse, advertiseCallback)
            Log.i(TAG, "BLE advertising started successfully")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start BLE advertising: ${e.message}")
        }
    }

    @SuppressLint("MissingPermission")
    private fun stopAdvertising() {
        try {
            advertiser?.stopAdvertising(advertiseCallback)
        } catch (_: Exception) {}
    }

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartSuccess(settingsInEffect: AdvertiseSettings?) {
            Log.i(TAG, "AdvertiseCallback onStartSuccess")
        }
        override fun onStartFailure(errorCode: Int) {
            Log.e(TAG, "AdvertiseCallback onStartFailure code: $errorCode")
        }
    }

    @SuppressLint("MissingPermission")
    private fun startGattServer() {
        val manager = bluetoothManager ?: return
        try {
            gattServer = manager.openGattServer(this, gattServerCallback)
        } catch (e: Exception) {
            Log.e(TAG, "Failed to open GATT server: ${e.message}")
            return
        }

        val service = BluetoothGattService(SERVICE_UUID, BluetoothGattService.SERVICE_TYPE_PRIMARY)

        val writeChar = BluetoothGattCharacteristic(
            CHAR_WRITE_UUID,
            BluetoothGattCharacteristic.PROPERTY_WRITE or BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE,
            BluetoothGattCharacteristic.PERMISSION_WRITE
        )

        val notifyChar = BluetoothGattCharacteristic(
            CHAR_NOTIFY_UUID,
            BluetoothGattCharacteristic.PROPERTY_NOTIFY or BluetoothGattCharacteristic.PROPERTY_READ,
            BluetoothGattCharacteristic.PERMISSION_READ
        )

        val cccd = BluetoothGattDescriptor(CCCD_UUID, BluetoothGattDescriptor.PERMISSION_WRITE or BluetoothGattDescriptor.PERMISSION_READ)
        notifyChar.addDescriptor(cccd)

        service.addCharacteristic(writeChar)
        service.addCharacteristic(notifyChar)
        gattServer?.addService(service)
        Log.i(TAG, "GATT server configured and running")
    }

    @SuppressLint("MissingPermission")
    private fun closeGattServer() {
        try {
            gattServer?.close()
            gattServer = null
        } catch (_: Exception) {}
    }

    private val gattServerCallback = object : BluetoothGattServerCallback() {
        @SuppressLint("MissingPermission")
        override fun onConnectionStateChange(device: BluetoothDevice?, status: Int, newState: Int) {
            val dev = device ?: return
            val addr = dev.address
            Log.i(TAG, "GattServer onConnectionStateChange: $addr -> $newState (status: $status)")

            if (newState == BluetoothProfile.STATE_CONNECTED) {
                serverConnectedDevices.add(dev)
                listener?.onPeerConnected(addr)
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                serverConnectedDevices.remove(dev)
                if (!outgoingGatts.containsKey(addr)) {
                    listener?.onPeerDisconnected(addr)
                }
            }
        }

        @SuppressLint("MissingPermission")
        override fun onCharacteristicWriteRequest(
            device: BluetoothDevice?,
            requestId: Int,
            characteristic: BluetoothGattCharacteristic?,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray?
        ) {
            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, value)
            }
            if (value != null && device != null) {
                listener?.onFrameReceived(device.address, value)
            }
        }

        @SuppressLint("MissingPermission")
        override fun onDescriptorWriteRequest(
            device: BluetoothDevice?,
            requestId: Int,
            descriptor: BluetoothGattDescriptor?,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray?
        ) {
            if (descriptor?.uuid == CCCD_UUID && device != null) {
                serverConnectedDevices.add(device)
            }
            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, value)
            }
        }
    }

    @SuppressLint("MissingPermission")
    private fun startScanning() {
        bluetoothManager = getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        bluetoothAdapter = bluetoothManager?.adapter
        scanner = bluetoothAdapter?.bluetoothLeScanner
        if (scanner == null) {
            Log.e(TAG, "BLE scanner not available")
            return
        }

        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            .build()

        try {
            // Software filtering avoids OEM HAL hardware filter drops on OnePlus/Xiaomi
            scanner?.startScan(null, settings, scanCallback)
            isScanning = true
            Log.i(TAG, "BLE scan started (software-filtered)")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start BLE scan: ${e.message}")
        }
    }

    @SuppressLint("MissingPermission")
    private fun stopScanning() {
        if (!isScanning) return
        try {
            scanner?.stopScan(scanCallback)
            isScanning = false
        } catch (_: Exception) {}
    }

    private val scanRefreshRunnable = object : Runnable {
        @SuppressLint("MissingPermission")
        override fun run() {
            if (hasRequiredPermissions() && isScanning) {
                try {
                    scanner?.stopScan(scanCallback)
                    mainHandler.postDelayed({
                        if (isScanning) {
                            bluetoothAdapter = bluetoothManager?.adapter
                            scanner = bluetoothAdapter?.bluetoothLeScanner
                            val settings = ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build()
                            scanner?.startScan(null, settings, scanCallback)
                        }
                    }, 200)
                } catch (_: Exception) {}
            }
            mainHandler.postDelayed(this, 15000)
        }
    }

    private fun startPeriodicScanRefresh() {
        mainHandler.removeCallbacks(scanRefreshRunnable)
        mainHandler.postDelayed(scanRefreshRunnable, 15000)
    }

    private fun stopPeriodicScanRefresh() {
        mainHandler.removeCallbacks(scanRefreshRunnable)
    }

    private val scanCallback = object : ScanCallback() {
        @SuppressLint("MissingPermission")
        override fun onScanResult(callbackType: Int, result: ScanResult?) {
            val device = result?.device ?: return
            val record = result.scanRecord ?: return

            // Robust software match: check serviceUuids, serviceData keys, or raw bytes
            val uuids = record.serviceUuids
            val hasUuid = uuids != null && uuids.any {
                it.uuid == SERVICE_UUID || it.uuid.toString().uppercase().contains("FD6F")
            }

            val hasServiceData = record.serviceData.keys.any {
                it.uuid == SERVICE_UUID || it.uuid.toString().uppercase().contains("FD6F")
            }

            val rawBytes = record.bytes ?: ByteArray(0)
            var hasRawUuid = false
            for (i in 0 until (rawBytes.size - 1)) {
                if (rawBytes[i] == 0x6F.toByte() && rawBytes[i + 1] == 0xFD.toByte()) {
                    hasRawUuid = true
                    break
                }
            }

            if (!hasUuid && !hasServiceData && !hasRawUuid) return

            val addr = device.address

            // Prevent redundant connection if already central, already peripheral, or currently connecting
            if (outgoingGatts.containsKey(addr) || connectingDevices.contains(addr) || serverConnectedDevices.any { it.address == addr }) {
                return
            }

            Log.i(TAG, "Discovered mesh peer $addr. Connecting GATT client...")
            connectingDevices.add(addr)

            // Connection timeout fallback
            mainHandler.postDelayed({
                connectingDevices.remove(addr)
            }, 8000)

            mainHandler.post {
                device.connectGatt(
                    this@BitchatBleService,
                    false,
                    gattClientCallback,
                    BluetoothDevice.TRANSPORT_LE
                )
            }
        }

        override fun onScanFailed(errorCode: Int) {
            Log.e(TAG, "Scan failed with error code: $errorCode")
        }
    }

    private val gattClientCallback = object : BluetoothGattCallback() {
        @SuppressLint("MissingPermission")
        override fun onConnectionStateChange(gatt: BluetoothGatt?, status: Int, newState: Int) {
            val g = gatt ?: return
            val addr = g.device.address
            connectingDevices.remove(addr)

            Log.i(TAG, "GattClient onConnectionStateChange: $addr -> $newState (status: $status)")

            if (newState == BluetoothProfile.STATE_CONNECTED && status == BluetoothGatt.GATT_SUCCESS) {
                outgoingGatts[addr] = g
                g.requestMtu(512)
                mainHandler.postDelayed({
                    g.discoverServices()
                }, 300)
                listener?.onPeerConnected(addr)
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED || status != BluetoothGatt.GATT_SUCCESS) {
                outgoingGatts.remove(addr)
                try {
                    g.disconnect()
                    g.close()
                } catch (_: Exception) {}
                if (!serverConnectedDevices.any { it.address == addr }) {
                    listener?.onPeerDisconnected(addr)
                }
            }
        }

        @SuppressLint("MissingPermission")
        override fun onServicesDiscovered(gatt: BluetoothGatt?, status: Int) {
            if (status != BluetoothGatt.GATT_SUCCESS) return
            val g = gatt ?: return
            val service = g.getService(SERVICE_UUID) ?: return
            val notifyChar = service.getCharacteristic(CHAR_NOTIFY_UUID) ?: return

            g.setCharacteristicNotification(notifyChar, true)
            val cccd = notifyChar.getDescriptor(CCCD_UUID) ?: return

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                g.writeDescriptor(cccd, BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE)
            } else {
                @Suppress("DEPRECATION")
                cccd.value = BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
                @Suppress("DEPRECATION")
                g.writeDescriptor(cccd)
            }
            Log.i(TAG, "Subscribed to peer notifications: ${g.device.address}")
        }

        // Support for pre-Android 13
        @Deprecated("Deprecated in Java")
        override fun onCharacteristicChanged(gatt: BluetoothGatt?, characteristic: BluetoothGattCharacteristic?) {
            val addr = gatt?.device?.address ?: return
            val data = characteristic?.value ?: return
            listener?.onFrameReceived(addr, data)
        }

        // Support for Android 13+ (API 33+)
        override fun onCharacteristicChanged(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            value: ByteArray
        ) {
            val addr = gatt.device.address
            listener?.onFrameReceived(addr, value)
        }
    }

    /**
     * Transmit frame to peer(s) bidirectionally.
     * Uses outgoing GATT client writes AND incoming GATT server notifications.
     */
    @SuppressLint("MissingPermission")
    fun writeFrame(targetAddress: String?, data: ByteArray) {
        var sentCount = 0

        // 1. Send via outgoing GATT client connections (we are central)
        for ((addr, gatt) in outgoingGatts) {
            if (targetAddress == null || targetAddress == addr) {
                val service = gatt.getService(SERVICE_UUID)
                val writeChar = service?.getCharacteristic(CHAR_WRITE_UUID)
                if (writeChar != null) {
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                        gatt.writeCharacteristic(writeChar, data, BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE)
                    } else {
                        @Suppress("DEPRECATION")
                        writeChar.value = data
                        @Suppress("DEPRECATION")
                        writeChar.writeType = BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
                        @Suppress("DEPRECATION")
                        gatt.writeCharacteristic(writeChar)
                    }
                    sentCount++
                }
            }
        }

        // 2. Send via GATT server notifications (we are peripheral to connected centrals)
        val notifyChar = gattServer?.getService(SERVICE_UUID)?.getCharacteristic(CHAR_NOTIFY_UUID)
        if (notifyChar != null) {
            for (dev in serverConnectedDevices) {
                if (targetAddress == null || targetAddress == dev.address) {
                    try {
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                            gattServer?.notifyCharacteristicChanged(dev, notifyChar, false, data)
                        } else {
                            @Suppress("DEPRECATION")
                            notifyChar.value = data
                            @Suppress("DEPRECATION")
                            gattServer?.notifyCharacteristicChanged(dev, notifyChar, false)
                        }
                        sentCount++
                    } catch (e: Exception) {
                        Log.w(TAG, "Notification write failed: ${e.message}")
                    }
                }
            }
        }

        Log.i(TAG, "writeFrame dispatched to $sentCount channels (target: $targetAddress)")
    }

    private fun hexStringToByteArray(s: String): ByteArray {
        val clean = if (s.length % 2 != 0) "0$s" else s
        val len = clean.length
        val data = ByteArray(len / 2)
        var i = 0
        while (i < len) {
            data[i / 2] = ((Character.digit(clean[i], 16) shl 4) + Character.digit(clean[i + 1], 16)).toByte()
            i += 2
        }
        return data
    }

    override fun onDestroy() {
        try {
            unregisterReceiver(bluetoothReceiver)
        } catch (_: Exception) {}
        stopMesh()
        super.onDestroy()
    }
}
