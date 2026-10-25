package com.bitchat.bitchat2

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

class KeystoreLockedException(message: String, cause: Throwable? = null) : Exception(message, cause)

/**
 * Service-owned hardware-backed Key Provider for Bitchat SQLite outbox encryption.
 *
 * Architecture:
 * 1. Root Key Encryption Key (KEK) is generated inside Android Keystore (`AndroidKeyStore`),
 *    using AES-GCM-256 with hardware backing (StrongBox/TEE).
 * 2. A 32-byte Data Encryption Key (DEK) is generated via CSPRNG (`SecureRandom`),
 *    wrapped using KEK, and stored as `bitchat_dek.enc` in app filesDir.
 * 3. File backup exclusions: `bitchat_dek.enc` and `bitchat_outbox.db` are excluded
 *    from automatic OS backup via `android:allowBackup="false"`.
 * 4. Explicit failure handling: Throws `KeystoreLockedException` if device credentials or key are unavailable.
 */
object AndroidKeystoreProvider {
    private const val KEYSTORE_PROVIDER = "AndroidKeyStore"
    private const val KEK_ALIAS = "bitchat_outbox_kek"
    private const val WRAPPED_DEK_FILE = "bitchat_dek.enc"
    private const val GCM_TAG_LENGTH = 128
    private const val GCM_IV_LENGTH = 12
    private const val DEK_LENGTH = 32

    @Synchronized
    fun getOrCreateDek(context: Context): ByteArray {
        try {
            val keyStore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
            val kek = getOrCreateKek(keyStore)

            val dekFile = File(context.filesDir, WRAPPED_DEK_FILE)
            return if (dekFile.exists() && dekFile.length() > GCM_IV_LENGTH) {
                unwrapDek(dekFile.readBytes(), kek)
            } else {
                val newDek = ByteArray(DEK_LENGTH).apply { SecureRandom().nextBytes(this) }
                val wrapped = wrapDek(newDek, kek)
                dekFile.writeBytes(wrapped)
                newDek
            }
        } catch (e: Exception) {
            throw KeystoreLockedException("Failed to access Android Keystore DEK: ${e.message}", e)
        }
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

        val entry = keyStore.getEntry(KEK_ALIAS, null) as? KeyStore.SecretKeyEntry
            ?: throw IllegalStateException("Keystore alias $KEK_ALIAS is not a SecretKeyEntry")
        return entry.secretKey
    }

    private fun wrapDek(dek: ByteArray, kek: SecretKey): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, kek)
        val iv = cipher.iv
        val ciphertext = cipher.doFinal(dek)
        return iv + ciphertext
    }

    private fun unwrapDek(wrapped: ByteArray, kek: SecretKey): ByteArray {
        val iv = wrapped.copyOfRange(0, GCM_IV_LENGTH)
        val ciphertext = wrapped.copyOfRange(GCM_IV_LENGTH, wrapped.size)
        val spec = GCMParameterSpec(GCM_TAG_LENGTH, iv)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, kek, spec)
        return cipher.doFinal(ciphertext)
    }
}
