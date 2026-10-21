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
import android.os.Binder
import android.os.Build
import android.os.IBinder
import android.os.ParcelUuid
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.util.*
import java.util.concurrent.ConcurrentHashMap

/**
 * Dual-role BLE Mesh Foreground Service.
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
    private var bluetoothManager: BluetoothManager? = null
    private var bluetoothAdapter: BluetoothAdapter? = null

    // Peripheral
    private var advertiser: BluetoothLeAdvertiser? = null
    private var gattServer: BluetoothGattServer? = null

    // Central
    private var scanner: BluetoothLeScanner? = null
    private val connectedCentrals = ConcurrentHashMap<String, BluetoothGatt>()
    private val subscribedDevices = ConcurrentHashMap.newKeySet<BluetoothDevice>()

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
    }

    fun hasRequiredPermissions(): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val scan = ContextCompat.checkSelfPermission(this, Manifest.permission.BLUETOOTH_SCAN) == PackageManager.PERMISSION_GRANTED
            val connect = ContextCompat.checkSelfPermission(this, Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
            val advertise = ContextCompat.checkSelfPermission(this, Manifest.permission.BLUETOOTH_ADVERTISE) == PackageManager.PERMISSION_GRANTED
            if (!scan || !connect || !advertise) return false
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
    fun startMesh(localShortIdHex: String) {
        if (!hasRequiredPermissions()) {
            Log.w(TAG, "Cannot start mesh: missing Bluetooth permissions")
            return
        }
        startServiceInForeground()

        val adapter = bluetoothAdapter ?: return
        if (!adapter.isEnabled) return

        startGattServer()
        startAdvertising(localShortIdHex)
        startScanning()
    }

    @SuppressLint("MissingPermission")
    fun stopMesh() {
        stopAdvertising()
        stopScanning()
        closeGattServer()
    }

    @SuppressLint("MissingPermission")
    private fun startAdvertising(localShortIdHex: String) {
        advertiser = bluetoothAdapter?.bluetoothLeAdvertiser ?: return

        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_HIGH)
            .setConnectable(true)
            .build()

        val pUuid = ParcelUuid(SERVICE_UUID)
        val shortBytes = hexStringToByteArray(localShortIdHex)

        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .addServiceUuid(pUuid)
            .addServiceData(pUuid, shortBytes)
            .build()

        advertiser?.startAdvertising(settings, data, advertiseCallback)
    }

    @SuppressLint("MissingPermission")
    private fun stopAdvertising() {
        try {
            advertiser?.stopAdvertising(advertiseCallback)
        } catch (_: Exception) {}
    }

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartSuccess(settingsInEffect: AdvertiseSettings?) {}
        override fun onStartFailure(errorCode: Int) {}
    }

    @SuppressLint("MissingPermission")
    private fun startGattServer() {
        val manager = bluetoothManager ?: return
        gattServer = manager.openGattServer(this, gattServerCallback)

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
            val addr = device?.address ?: return
            if (newState == BluetoothProfile.STATE_CONNECTED) {
                listener?.onPeerConnected(addr)
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                subscribedDevices.remove(device)
                listener?.onPeerDisconnected(addr)
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
                subscribedDevices.add(device)
            }
            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, value)
            }
        }
    }

    @SuppressLint("MissingPermission")
    private fun startScanning() {
        scanner = bluetoothAdapter?.bluetoothLeScanner ?: return

        val filter = ScanFilter.Builder()
            .setServiceUuid(ParcelUuid(SERVICE_UUID))
            .build()

        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            .build()

        scanner?.startScan(listOf(filter), settings, scanCallback)
    }

    @SuppressLint("MissingPermission")
    private fun stopScanning() {
        try {
            scanner?.stopScan(scanCallback)
        } catch (_: Exception) {}
    }

    private val scanCallback = object : ScanCallback() {
        @SuppressLint("MissingPermission")
        override fun onScanResult(callbackType: Int, result: ScanResult?) {
            val device = result?.device ?: return
            val addr = device.address

            if (!connectedCentrals.containsKey(addr)) {
                device.connectGatt(this@BitchatBleService, false, gattClientCallback, BluetoothDevice.TRANSPORT_LE)
            }
        }
    }

    private val gattClientCallback = object : BluetoothGattCallback() {
        @SuppressLint("MissingPermission")
        override fun onConnectionStateChange(gatt: BluetoothGatt?, status: Int, newState: Int) {
            val addr = gatt?.device?.address ?: return
            if (newState == BluetoothProfile.STATE_CONNECTED) {
                connectedCentrals[addr] = gatt
                gatt.requestMtu(512)
                gatt.discoverServices()
                listener?.onPeerConnected(addr)
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                connectedCentrals.remove(addr)
                listener?.onPeerDisconnected(addr)
                try { gatt.close() } catch (_: Exception) {}
            }
        }

        @SuppressLint("MissingPermission")
        override fun onServicesDiscovered(gatt: BluetoothGatt?, status: Int) {
            val service = gatt?.getService(SERVICE_UUID) ?: return
            val notifyChar = service.getCharacteristic(CHAR_NOTIFY_UUID) ?: return

            gatt.setCharacteristicNotification(notifyChar, true)
            val cccd = notifyChar.getDescriptor(CCCD_UUID)
            if (cccd != null) {
                cccd.value = BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
                gatt.writeDescriptor(cccd)
            }
        }

        override fun onCharacteristicChanged(gatt: BluetoothGatt?, characteristic: BluetoothGattCharacteristic?) {
            val addr = gatt?.device?.address ?: return
            val data = characteristic?.value ?: return
            listener?.onFrameReceived(addr, data)
        }
    }

    /**
     * Transmit frame to peer(s).
     */
    @SuppressLint("MissingPermission")
    fun writeFrame(targetAddress: String?, data: ByteArray) {
        if (targetAddress != null) {
            val gatt = connectedCentrals[targetAddress]
            if (gatt != null) {
                val service = gatt.getService(SERVICE_UUID)
                val writeChar = service?.getCharacteristic(CHAR_WRITE_UUID)
                if (writeChar != null) {
                    writeChar.value = data
                    writeChar.writeType = BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
                    gatt.writeCharacteristic(writeChar)
                }
            }
        } else {
            // Broadcast: notify all subscribed peripherals and write to all connected centrals
            val notifyChar = gattServer?.getService(SERVICE_UUID)?.getCharacteristic(CHAR_NOTIFY_UUID)
            if (notifyChar != null) {
                notifyChar.value = data
                for (dev in subscribedDevices) {
                    try {
                        gattServer?.notifyCharacteristicChanged(dev, notifyChar, false)
                    } catch (_: Exception) {}
                }
            }

            for ((_, gatt) in connectedCentrals) {
                val service = gatt.getService(SERVICE_UUID)
                val writeChar = service?.getCharacteristic(CHAR_WRITE_UUID)
                if (writeChar != null) {
                    writeChar.value = data
                    writeChar.writeType = BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
                    gatt.writeCharacteristic(writeChar)
                }
            }
        }
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

    override fun onDestroy() {
        stopMesh()
        super.onDestroy()
    }
}
