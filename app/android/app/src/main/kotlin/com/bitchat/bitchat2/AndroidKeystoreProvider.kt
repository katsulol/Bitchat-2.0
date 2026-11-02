package com.bitchat.bitchat2

// CRYPTO-REVIEW:
// What it does: Service-owned Android Keystore KEK/DEK lifecycle with versioned bounded wrapped-key
// records, atomic durable writes via AtomicFile, fail-closed protection when an encrypted database
// already exists, and accurate hardware security level inspection (StrongBox / TEE / Software / Unknown).
// Primitives / APIs: `AndroidKeyStore` (AES/GCM/NoPadding 256-bit KEK), `SecureRandom` (256-bit DEK),
// `android.security.keystore.KeyInfo`, `android.util.AtomicFile`.
// Reviewer checklist:
// 1. Wrapped record format is versioned and strictly length-checked (69 bytes v1; legacy 60-byte v0 migrated safely).
// 2. If `bitchat_outbox.db` exists and wrapped DEK or KEK is missing/corrupted, `getOrCreateDek` fails closed
//    with `KeystoreLockedException` and NEVER silently overwrites the key.
// 3. `AtomicFile` ensures partial writes / crash mid-write never corrupt the existing wrapped key.
// 4. Hardware backing is queried via `KeyInfo` rather than assumed from `"AndroidKeyStore"` provider name.

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import java.io.File
import java.nio.ByteBuffer
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

class KeystoreLockedException(message: String, cause: Throwable? = null) : Exception(message, cause)

object AndroidKeystoreProvider {
    private const val KEYSTORE_PROVIDER = "AndroidKeyStore"
    private const val KEK_ALIAS = "bitchat_outbox_kek"
    private const val WRAPPED_DEK_FILE = "bitchat_dek.enc"
    private const val OUTBOX_DB_FILE = "bitchat_outbox.db"
    private const val GCM_TAG_LENGTH = 128
    private const val GCM_IV_LENGTH = 12
    private const val DEK_LENGTH = 32
    private const val CIPHERTEXT_LENGTH = DEK_LENGTH + (GCM_TAG_LENGTH / 8) // 48 bytes
    private const val LEGACY_RECORD_LEN = GCM_IV_LENGTH + CIPHERTEXT_LENGTH // 60 bytes
    // V1 Header: Magic "BC2K" (4B) + FormatVersion (1B) + KeyVersion (4B) + IV (12B) + Ciphertext (48B) = 69B
    private const val V1_RECORD_LEN = 4 + 1 + 4 + GCM_IV_LENGTH + CIPHERTEXT_LENGTH
    private val MAGIC = byteArrayOf('B'.code.toByte(), 'C'.code.toByte(), '2'.code.toByte(), 'K'.code.toByte())
    private const val FORMAT_VERSION_V1: Byte = 1
    private const val CURRENT_KEY_VERSION: Int = 1

    @Synchronized
    fun getOrCreateDek(context: Context): ByteArray {
        try {
            val keyStore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
            val dekFile = File(context.filesDir, WRAPPED_DEK_FILE)
            val dbFile = File(context.filesDir, OUTBOX_DB_FILE)
            val dbExists = dbFile.exists() && dbFile.length() > 0L

            if (dekFile.exists()) {
                if (!keyStore.containsAlias(KEK_ALIAS)) {
                    throw KeystoreLockedException(
                        "KEK alias '$KEK_ALIAS' missing while wrapped DEK file exists; failing closed"
                    )
                }
                val kek = getExistingKek(keyStore)
                val raw = AtomicFile(dekFile).readFully()
                return unwrapVersionedOrMigrate(dekFile, raw, kek)
            } else {
                // Fail closed if an encrypted SQLite outbox database already exists on disk!
                if (dbExists) {
                    throw KeystoreLockedException(
                        "Encrypted outbox database exists without wrapped DEK file; failing closed to prevent data loss"
                    )
                }
                val kek = getOrCreateKek(keyStore)
                val newDek = ByteArray(DEK_LENGTH).apply { SecureRandom().nextBytes(this) }
                val wrappedRecord = wrapVersionedDek(newDek, kek, CURRENT_KEY_VERSION)
                writeAtomic(dekFile, wrappedRecord)
                return newDek
            }
        } catch (e: KeystoreLockedException) {
            throw e
        } catch (e: Exception) {
            throw KeystoreLockedException("Failed to access Android Keystore DEK: ${e.message}", e)
        }
    }

    /**
     * Explicit user-authorized destructive reset when keystore state is unrecoverable.
     * Deletes wrapped DEK, SQLite database (plus WAL/SHM), and rotates KEK alias.
     */
    @Synchronized
    fun destructiveReset(context: Context) {
        File(context.filesDir, WRAPPED_DEK_FILE).delete()
        File(context.filesDir, "$WRAPPED_DEK_FILE.new").delete()
        File(context.filesDir, "$WRAPPED_DEK_FILE.bak").delete()
        File(context.filesDir, OUTBOX_DB_FILE).delete()
        File(context.filesDir, "$OUTBOX_DB_FILE-wal").delete()
        File(context.filesDir, "$OUTBOX_DB_FILE-shm").delete()
        try {
            val keyStore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
            if (keyStore.containsAlias(KEK_ALIAS)) {
                keyStore.deleteEntry(KEK_ALIAS)
            }
        } catch (_: Exception) {}
    }

    /**
     * Inspect actual hardware backing level of the Android Keystore KEK.
     * Returns one of: `"StrongBox"`, `"TEE"`, `"Software"`, `"Unknown"`.
     */
    @Synchronized
    fun getHardwareSecurityLevel(context: Context): String {
        return try {
            val keyStore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
            if (!keyStore.containsAlias(KEK_ALIAS)) {
                return "Unknown"
            }
            val kek = getExistingKek(keyStore)
            val factory = SecretKeyFactory.getInstance(kek.algorithm, KEYSTORE_PROVIDER)
            val keyInfo = factory.getKeySpec(kek, KeyInfo::class.java) as? KeyInfo ?: return "Unknown"

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                when (keyInfo.securityLevel) {
                    KeyProperties.SECURITY_LEVEL_STRONGBOX -> "StrongBox"
                    KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT -> "TEE"
                    KeyProperties.SECURITY_LEVEL_SOFTWARE -> "Software"
                    else -> "Unknown"
                }
            } else {
                @Suppress("DEPRECATION")
                if (keyInfo.isInsideSecureHardware) "TEE" else "Software"
            }
        } catch (_: Exception) {
            "Unknown"
        }
    }

    private fun getExistingKek(keyStore: KeyStore): SecretKey {
        val entry = keyStore.getEntry(KEK_ALIAS, null) as? KeyStore.SecretKeyEntry
            ?: throw KeystoreLockedException("Keystore alias $KEK_ALIAS is missing or not a SecretKeyEntry")
        return entry.secretKey
    }

    private fun getOrCreateKek(keyStore: KeyStore): SecretKey {
        if (!keyStore.containsAlias(KEK_ALIAS)) {
            val keyGenerator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE_PROVIDER)
            val spec = KeyGenParameterSpec.Builder(
                KEK_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setRandomizedEncryptionRequired(true)
                .build()

            keyGenerator.init(spec)
            keyGenerator.generateKey()
        }
        return getExistingKek(keyStore)
    }

    private fun wrapVersionedDek(dek: ByteArray, kek: SecretKey, keyVersion: Int): ByteArray {
        if (dek.size != DEK_LENGTH) {
            throw KeystoreLockedException("Invalid DEK length: ${dek.size}")
        }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, kek)
        val iv = cipher.iv
        if (iv == null || iv.size != GCM_IV_LENGTH) {
            throw KeystoreLockedException("Invalid AES-GCM IV length")
        }
        val ciphertext = cipher.doFinal(dek)
        if (ciphertext.size != CIPHERTEXT_LENGTH) {
            throw KeystoreLockedException("Invalid AES-GCM ciphertext length: ${ciphertext.size}")
        }

        val buf = ByteBuffer.allocate(V1_RECORD_LEN)
        buf.put(MAGIC)
        buf.put(FORMAT_VERSION_V1)
        buf.putInt(keyVersion)
        buf.put(iv)
        buf.put(ciphertext)
        return buf.array()
    }

    private fun unwrapVersionedOrMigrate(dekFile: File, raw: ByteArray, kek: SecretKey): ByteArray {
        if (raw.size == V1_RECORD_LEN) {
            for (i in 0..3) {
                if (raw[i] != MAGIC[i]) {
                    throw KeystoreLockedException("Corrupted wrapped DEK header magic")
                }
            }
            val fmtVer = raw[4]
            if (fmtVer != FORMAT_VERSION_V1) {
                throw KeystoreLockedException("Unsupported wrapped DEK format version: $fmtVer")
            }
            val iv = raw.copyOfRange(9, 9 + GCM_IV_LENGTH)
            val ciphertext = raw.copyOfRange(9 + GCM_IV_LENGTH, V1_RECORD_LEN)
            return decryptGcm(iv, ciphertext, kek)
        } else if (raw.size == LEGACY_RECORD_LEN) {
            // Safe migration from Phase 4 v0 60-byte wrapped key record
            val iv = raw.copyOfRange(0, GCM_IV_LENGTH)
            val ciphertext = raw.copyOfRange(GCM_IV_LENGTH, LEGACY_RECORD_LEN)
            val dek = decryptGcm(iv, ciphertext, kek)
            val upgraded = wrapVersionedDek(dek, kek, CURRENT_KEY_VERSION)
            writeAtomic(dekFile, upgraded)
            return dek
        } else {
            throw KeystoreLockedException(
                "Malformed wrapped DEK file length: ${raw.size} bytes (expected $V1_RECORD_LEN)"
            )
        }
    }

    private fun decryptGcm(iv: ByteArray, ciphertext: ByteArray, kek: SecretKey): ByteArray {
        val spec = GCMParameterSpec(GCM_TAG_LENGTH, iv)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, kek, spec)
        val plain = cipher.doFinal(ciphertext)
        if (plain.size != DEK_LENGTH) {
            plain.fill(0)
            throw KeystoreLockedException("Unwrapped DEK has unexpected length: ${plain.size}")
        }
        return plain
    }

    private fun writeAtomic(file: File, content: ByteArray) {
        val atomicFile = AtomicFile(file)
        val fos = atomicFile.startWrite()
        try {
            fos.write(content)
            fos.fd.sync()
            atomicFile.finishWrite(fos)
        } catch (e: Exception) {
            atomicFile.failWrite(fos)
            throw e
        }
    }
}
