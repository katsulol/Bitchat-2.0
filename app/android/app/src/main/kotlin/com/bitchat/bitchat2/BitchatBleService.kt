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
import java.io.ByteArrayOutputStream
import java.util.*
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ConcurrentLinkedQueue

/**
 * Enterprise Dual-role BLE Mesh Foreground Service.
 *
 * Implements:
 * 1. Link vs Peer separation (Tracks verified ShortId, prevents MAC rotation duplicates).
 * 2. Serialized GATT client write queues with onCharacteristicWrite acknowledgment.
 * 3. Transparent BLE packet chunking and reassembly for payloads crossing MTU boundary.
 * 4. Real MTU negotiation tracking via onMtuChanged on both Central and Peripheral paths.
 * 5. Per-client CCCD descriptor state management with in-memory value assignment.
 * 6. Single-path outbound dispatch (eliminates 2x duplicate delivery).
 * 7. Active timeout cleanup of zombie BluetoothGatt instances.
 * 8. Asynchronous onServiceAdded gating before advertising start.
 * 9. Complete onDestroy lifecycle teardown.
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

        private const val PKT_TYPE_SINGLE: Byte = 0x00
        private const val PKT_TYPE_CHUNK: Byte = 0x01
    }

    private val binder = LocalBinder()
    private val mainHandler = Handler(Looper.getMainLooper())
    private var bluetoothManager: BluetoothManager? = null
    private var bluetoothAdapter: BluetoothAdapter? = null

    // Peripheral
    private var advertiser: BluetoothLeAdvertiser? = null
    private var gattServer: BluetoothGattServer? = null
    private val serverConnectedDevices = ConcurrentHashMap.newKeySet<BluetoothDevice>()
    private val subscribedClients = ConcurrentHashMap.newKeySet<String>()

    // Central
    private var scanner: BluetoothLeScanner? = null
    private val outgoingGatts = ConcurrentHashMap<String, BluetoothGatt>()
    private val pendingGatts = ConcurrentHashMap<String, BluetoothGatt>()
    private val connectingDevices = ConcurrentHashMap.newKeySet<String>()

    // MTU tracking per address (default 23 -> 20 bytes payload)
    private val peerMtu = ConcurrentHashMap<String, Int>()

    // Write queues per client address: FIFO queue of chunks to transmit
    private val writeQueues = ConcurrentHashMap<String, ConcurrentLinkedQueue<ByteArray>>()
    private val isWriting = ConcurrentHashMap<String, Boolean>()

    // Reassembly buffer: map of "address:sessionId" -> ReassemblyState
    private class ReassemblyState(val total: Int) {
        val chunks = arrayOfNulls<ByteArray>(total)
        var receivedCount = 0
        var lastUpdated = System.currentTimeMillis()
    }
    private val reassemblyMap = ConcurrentHashMap<String, ReassemblyState>()

    // Peer Identity Mapping: MAC Address <-> ShortId Hex
    private val addrToShortId = ConcurrentHashMap<String, String>()
    private val shortIdToLinks = ConcurrentHashMap<String, MutableSet<String>>()

    // Local Short ID
    private var localShortIdHex: String = ""
    private var isScanning = false
    private var isGattServerReady = false

    // Callback listener for Flutter integration
    var listener: BleEventListener? = null

    interface BleEventListener {
        fun onPeerConnected(shortId: String)
        fun onPeerDisconnected(shortId: String)
        fun onFrameReceived(senderShortId: String, data: ByteArray)
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
                        if (localShortIdHex.isNotEmpty()) {
                            startMesh(localShortIdHex)
                        }
                    }, 500)
                } else if (state == BluetoothAdapter.STATE_OFF) {
                    Log.i(TAG, "Bluetooth switched to STATE_OFF. Halting radio...")
                    stopMeshInternal()
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

        stopMeshInternal()

        mainHandler.postDelayed({
            startGattServer()
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
        disconnectAllGatts()
        writeQueues.clear()
        isWriting.clear()
        reassemblyMap.clear()
        addrToShortId.clear()
        shortIdToLinks.clear()
        subscribedClients.clear()
        peerMtu.clear()
        isGattServerReady = false
    }

    @SuppressLint("MissingPermission")
    private fun disconnectAllGatts() {
        for ((_, gatt) in outgoingGatts) {
            try {
                gatt.disconnect()
                gatt.close()
            } catch (_: Exception) {}
        }
        outgoingGatts.clear()

        for ((_, gatt) in pendingGatts) {
            try {
                gatt.disconnect()
                gatt.close()
            } catch (_: Exception) {}
        }
        pendingGatts.clear()
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

        // Primary packet: Service UUID only (<21 bytes)
        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .addServiceUuid(pUuid)
            .build()

        // Scan response: Service UUID + 8-byte ShortId Service Data (<22 bytes)
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
        Log.i(TAG, "GATT server addService initiated...")
    }

    @SuppressLint("MissingPermission")
    private fun closeGattServer() {
        try {
            gattServer?.close()
            gattServer = null
        } catch (_: Exception) {}
    }

    private val gattServerCallback = object : BluetoothGattServerCallback() {
        override fun onServiceAdded(status: Int, service: BluetoothGattService?) {
            if (status == BluetoothGatt.GATT_SUCCESS) {
                isGattServerReady = true
                Log.i(TAG, "GATT server configured and confirmed ready. Starting radio...")
                mainHandler.post {
                    startAdvertising()
                    startScanning()
                    startPeriodicScanRefresh()
                }
            } else {
                Log.e(TAG, "Failed to add GATT service, status: $status")
            }
        }

        @SuppressLint("MissingPermission")
        override fun onConnectionStateChange(device: BluetoothDevice?, status: Int, newState: Int) {
            val dev = device ?: return
            val addr = dev.address
            Log.i(TAG, "GattServer onConnectionStateChange: $addr -> $newState (status: $status)")

            if (newState == BluetoothProfile.STATE_CONNECTED) {
                serverConnectedDevices.add(dev)
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                serverConnectedDevices.remove(dev)
                subscribedClients.remove(addr)
                peerMtu.remove(addr)
                removeLink(addr)
            }
        }

        override fun onMtuChanged(device: BluetoothDevice?, mtu: Int) {
            if (device != null) {
                peerMtu[device.address] = mtu
                Log.i(TAG, "GattServer negotiated MTU for ${device.address}: $mtu")
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
            if (characteristic?.uuid != CHAR_WRITE_UUID || offset != 0 || preparedWrite) {
                if (responseNeeded) {
                    gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_FAILURE, offset, null)
                }
                return
            }

            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, value)
            }

            if (value != null && device != null) {
                handleIncomingChunk(device.address, value)
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
            if (descriptor?.uuid != CCCD_UUID || offset != 0 || preparedWrite) {
                if (responseNeeded) {
                    gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_FAILURE, offset, null)
                }
                return
            }

            val dev = device ?: return
            val addr = dev.address

            if (Arrays.equals(value, BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE)) {
                subscribedClients.add(addr)
                descriptor.value = value
                Log.i(TAG, "Client $addr subscribed to GATT notifications")
            } else if (Arrays.equals(value, BluetoothGattDescriptor.DISABLE_NOTIFICATION_VALUE)) {
                subscribedClients.remove(addr)
                descriptor.value = value
                Log.i(TAG, "Client $addr unsubscribed from GATT notifications")
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
            .setReportDelay(0)
            .build()

        try {
            scanner?.startScan(null, settings, scanCallback)
            isScanning = true
            Log.i(TAG, "BLE scan started (exact software filter)")
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
                            val settings = ScanSettings.Builder()
                                .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                                .setReportDelay(0)
                                .build()
                            scanner?.startScan(null, settings, scanCallback)
                        }
                    }, 200)
                } catch (_: Exception) {}
            }
            mainHandler.postDelayed(this, 20000)
        }
    }

    private fun startPeriodicScanRefresh() {
        mainHandler.removeCallbacks(scanRefreshRunnable)
        mainHandler.postDelayed(scanRefreshRunnable, 20000)
    }

    private fun stopPeriodicScanRefresh() {
        mainHandler.removeCallbacks(scanRefreshRunnable)
    }

    private val scanCallback = object : ScanCallback() {
        @SuppressLint("MissingPermission")
        override fun onScanResult(callbackType: Int, result: ScanResult?) {
            val device = result?.device ?: return
            val record = result.scanRecord ?: return

            val pUuid = ParcelUuid(SERVICE_UUID)
            val hasServiceUuid = record.serviceUuids?.contains(pUuid) == true
            val shortBytes = record.serviceData[pUuid]
            val hasServiceData = shortBytes != null

            if (!hasServiceUuid && !hasServiceData) return

            val addr = device.address

            // Associate ShortId if advertised in scan response
            if (shortBytes != null && shortBytes.size == 8) {
                val peerShortId = byteArrayToHexString(shortBytes)
                if (peerShortId != localShortIdHex) {
                    associateLink(addr, peerShortId)
                }
            }

            // Prevent duplicate or redundant connections
            if (outgoingGatts.containsKey(addr) || pendingGatts.containsKey(addr) || connectingDevices.contains(addr)) {
                return
            }

            // If already connected as peripheral server, do not initiate redundant central client connection
            if (serverConnectedDevices.any { it.address == addr }) {
                return
            }

            Log.i(TAG, "Discovered mesh peer $addr. Initiating GATT connection...")
            connectingDevices.add(addr)

            val gattHandle = device.connectGatt(
                this@BitchatBleService,
                false,
                gattClientCallback,
                BluetoothDevice.TRANSPORT_LE
            )
            if (gattHandle != null) {
                pendingGatts[addr] = gattHandle
            }

            // Connection timeout fallback: Actively cancel and close zombie GATT handle
            mainHandler.postDelayed({
                if (connectingDevices.remove(addr)) {
                    val pending = pendingGatts.remove(addr)
                    if (pending != null) {
                        try {
                            pending.disconnect()
                            pending.close()
                            Log.w(TAG, "Connection timeout to $addr. Closed pending GATT.")
                        } catch (_: Exception) {}
                    }
                }
            }, 8000)
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
            pendingGatts.remove(addr)

            Log.i(TAG, "GattClient onConnectionStateChange: $addr -> $newState (status: $status)")

            if (newState == BluetoothProfile.STATE_CONNECTED && status == BluetoothGatt.GATT_SUCCESS) {
                outgoingGatts[addr] = g
                // Request MTU negotiation before discovery
                g.requestMtu(512)
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED || status != BluetoothGatt.GATT_SUCCESS) {
                outgoingGatts.remove(addr)
                peerMtu.remove(addr)
                writeQueues.remove(addr)
                isWriting.remove(addr)
                try {
                    g.disconnect()
                    g.close()
                } catch (_: Exception) {}
                removeLink(addr)
            }
        }

        @SuppressLint("MissingPermission")
        override fun onMtuChanged(gatt: BluetoothGatt?, mtu: Int, status: Int) {
            val g = gatt ?: return
            val addr = g.device.address
            peerMtu[addr] = if (status == BluetoothGatt.GATT_SUCCESS) mtu else 23
            Log.i(TAG, "GattClient negotiated MTU for $addr: ${peerMtu[addr]}. Discovering services...")
            mainHandler.post {
                g.discoverServices()
            }
        }

        @SuppressLint("MissingPermission")
        override fun onServicesDiscovered(gatt: BluetoothGatt?, status: Int) {
            if (status != BluetoothGatt.GATT_SUCCESS) return
            val g = gatt ?: return
            val addr = g.device.address
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
            Log.i(TAG, "Subscribed to peer notifications: $addr")

            // Verified ready link
            val shortId = addrToShortId[addr] ?: addr.replace(":", "").takeLast(16).uppercase()
            associateLink(addr, shortId)
        }

        override fun onCharacteristicWrite(
            gatt: BluetoothGatt?,
            characteristic: BluetoothGattCharacteristic?,
            status: Int
        ) {
            val addr = gatt?.device?.address ?: return
            isWriting[addr] = false
            // Process next chunk in queue
            drainWriteQueue(addr)
        }

        @Deprecated("Deprecated in Java")
        override fun onCharacteristicChanged(gatt: BluetoothGatt?, characteristic: BluetoothGattCharacteristic?) {
            val addr = gatt?.device?.address ?: return
            val data = characteristic?.value ?: return
            handleIncomingChunk(addr, data)
        }

        override fun onCharacteristicChanged(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            value: ByteArray
        ) {
            val addr = gatt.device.address
            handleIncomingChunk(addr, value)
        }
    }

    private fun associateLink(addr: String, shortId: String) {
        if (shortId == localShortIdHex) return
        addrToShortId[addr] = shortId
        val links = shortIdToLinks.computeIfAbsent(shortId) { ConcurrentHashMap.newKeySet() }
        val isFirstLink = links.isEmpty()
        links.add(addr)

        if (isFirstLink) {
            Log.i(TAG, "Peer connected: ShortId $shortId via link $addr")
            listener?.onPeerConnected(shortId)
        }
    }

    private fun removeLink(addr: String) {
        val shortId = addrToShortId.remove(addr) ?: return
        val links = shortIdToLinks[shortId]
        if (links != null) {
            links.remove(addr)
            if (links.isEmpty()) {
                shortIdToLinks.remove(shortId)
                Log.i(TAG, "Peer disconnected: ShortId $shortId (all links closed)")
                listener?.onPeerDisconnected(shortId)
            }
        }
    }

    /**
     * Reassembles incoming BLE chunks.
     * Byte 0: 0x00 = Single complete packet, 0x01 = Chunk [0x01, seq_u8, total_u8, ...data]
     */
    private fun handleIncomingChunk(addr: String, data: ByteArray) {
        if (data.isEmpty()) return
        val senderShortId = addrToShortId[addr] ?: addr.replace(":", "").takeLast(16).uppercase()

        when (data[0]) {
            PKT_TYPE_SINGLE -> {
                val payload = data.copyOfRange(1, data.size)
                listener?.onFrameReceived(senderShortId, payload)
            }
            PKT_TYPE_CHUNK -> {
                if (data.size < 4) return
                val seq = data[1].toInt() and 0xFF
                val total = data[2].toInt() and 0xFF
                val chunkData = data.copyOfRange(3, data.size)

                val key = "$addr:$total"
                val state = reassemblyMap.compute(key) { _, current ->
                    val s = if (current != null && current.total == total) current else ReassemblyState(total)
                    if (seq < total && s.chunks[seq] == null) {
                        s.chunks[seq] = chunkData
                        s.receivedCount++
                    }
                    s.lastUpdated = System.currentTimeMillis()
                    s
                }

                if (state != null && state.receivedCount == state.total) {
                    reassemblyMap.remove(key)
                    val out = ByteArrayOutputStream()
                    for (c in state.chunks) {
                        if (c != null) out.write(c)
                    }
                    val fullPayload = out.toByteArray()
                    listener?.onFrameReceived(senderShortId, fullPayload)
                }
            }
        }

        // Clean stale reassemblies (>10s)
        val now = System.currentTimeMillis()
        reassemblyMap.entries.removeIf { now - it.value.lastUpdated > 10000 }
    }

    /**
     * Transmit frame to peer(s) using Single-Path dispatch (Client write prioritized over notification).
     * Automatically chunks payloads that exceed negotiated MTU - 3 bytes.
     */
    @SuppressLint("MissingPermission")
    fun writeFrame(targetShortId: String?, data: ByteArray) {
        var sentCount = 0

        // Find candidate peer links
        val candidateAddrs = if (targetShortId != null) {
            shortIdToLinks[targetShortId]?.toList() ?: emptyList()
        } else {
            addrToShortId.keys.toList()
        }

        val contactedAddrs = HashSet<String>()

        // 1. Client GATT write priority (Single-path: prevents 2x duplicate delivery)
        for (addr in candidateAddrs) {
            val gatt = outgoingGatts[addr]
            if (gatt != null) {
                enqueueGattWrite(addr, gatt, data)
                contactedAddrs.add(addr)
                sentCount++
            }
        }

        // 2. Server notification fallback for devices connected to our GATT server not reached via Central
        val notifyChar = gattServer?.getService(SERVICE_UUID)?.getCharacteristic(CHAR_NOTIFY_UUID)
        if (notifyChar != null) {
            for (dev in serverConnectedDevices) {
                val addr = dev.address
                if (!contactedAddrs.contains(addr) && subscribedClients.contains(addr)) {
                    if (targetShortId == null || addrToShortId[addr] == targetShortId) {
                        enqueueServerNotification(dev, notifyChar, data)
                        sentCount++
                    }
                }
            }
        }

        Log.i(TAG, "writeFrame dispatched to $sentCount channels (target: $targetShortId)")
    }

    @SuppressLint("MissingPermission")
    private fun enqueueGattWrite(addr: String, gatt: BluetoothGatt, frameData: ByteArray) {
        val mtu = peerMtu[addr] ?: 23
        val maxPayload = (mtu - 3).coerceAtLeast(18)
        val chunks = splitIntoBlePackets(frameData, maxPayload)

        val queue = writeQueues.computeIfAbsent(addr) { ConcurrentLinkedQueue() }
        queue.addAll(chunks)

        if (isWriting[addr] != true) {
            drainWriteQueue(addr)
        }
    }

    @SuppressLint("MissingPermission")
    private fun drainWriteQueue(addr: String) {
        val gatt = outgoingGatts[addr] ?: return
        val queue = writeQueues[addr] ?: return
        val chunk = queue.poll() ?: return

        val service = gatt.getService(SERVICE_UUID)
        val writeChar = service?.getCharacteristic(CHAR_WRITE_UUID) ?: return

        isWriting[addr] = true
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            gatt.writeCharacteristic(writeChar, chunk, BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT)
        } else {
            @Suppress("DEPRECATION")
            writeChar.value = chunk
            @Suppress("DEPRECATION")
            writeChar.writeType = BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT
            @Suppress("DEPRECATION")
            gatt.writeCharacteristic(writeChar)
        }
    }

    @SuppressLint("MissingPermission")
    private fun enqueueServerNotification(
        dev: BluetoothDevice,
        notifyChar: BluetoothGattCharacteristic,
        frameData: ByteArray
    ) {
        val mtu = peerMtu[dev.address] ?: 23
        val maxPayload = (mtu - 3).coerceAtLeast(18)
        val chunks = splitIntoBlePackets(frameData, maxPayload)

        for (chunk in chunks) {
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    gattServer?.notifyCharacteristicChanged(dev, notifyChar, false, chunk)
                } else {
                    @Suppress("DEPRECATION")
                    notifyChar.value = chunk
                    @Suppress("DEPRECATION")
                    gattServer?.notifyCharacteristicChanged(dev, notifyChar, false)
                }
            } catch (e: Exception) {
                Log.w(TAG, "Notification write failed: ${e.message}")
            }
        }
    }

    private fun splitIntoBlePackets(data: ByteArray, maxPayload: Int): List<ByteArray> {
        val result = mutableListOf<ByteArray>()
        if (data.size + 1 <= maxPayload) {
            // Fits in single packet
            val packet = ByteArray(data.size + 1)
            packet[0] = PKT_TYPE_SINGLE
            System.arraycopy(data, 0, packet, 1, data.size)
            result.add(packet)
            return result
        }

        // Multi-packet chunking
        val chunkCapacity = maxPayload - 3 // [0x01, seq, total]
        val totalChunks = (data.size + chunkCapacity - 1) / chunkCapacity
        for (i in 0 until totalChunks) {
            val start = i * chunkCapacity
            val end = (start + chunkCapacity).coerceAtMost(data.size)
            val chunkLen = end - start
            val packet = ByteArray(chunkLen + 3)
            packet[0] = PKT_TYPE_CHUNK
            packet[1] = i.toByte()
            packet[2] = totalChunks.toByte()
            System.arraycopy(data, start, packet, 3, chunkLen)
            result.add(packet)
        }
        return result
    }

    private fun hexStringToByteArray(s: String): ByteArray {
        val len = s.length
        val data = ByteArray(len / 2)
        var i = 0
        while (i < len) {
            data[i / 2] = ((Character.digit(s[i], 16) shl 4) + Character.digit(s[i + 1], 16)).toByte()
            i += 2
        }
        return data
    }

    private fun byteArrayToHexString(bytes: ByteArray): String {
        val sb = StringBuilder()
        for (b in bytes) {
            sb.append(String.format("%02X", b))
        }
        return sb.toString()
    }

    override fun onDestroy() {
        stopMesh()
        try {
            unregisterReceiver(bluetoothReceiver)
        } catch (_: Exception) {}
        super.onDestroy()
    }
}
