package expo.modules.onideviceattestation

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.Base64
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import java.math.BigInteger
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.MessageDigest
import java.security.PrivateKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec

private const val KEYSTORE = "AndroidKeyStore"
private const val ALIAS = "onicode.push.device"

class OniDeviceAttestationModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("OniDeviceAttestation")

    Function("isHardwareAttestationSupported") { true }

    AsyncFunction("attestHardwareKey") { challenge: String ->
      val publicKey = generateKey(sha256(challenge))
      val chain = keyStore().getCertificateChain(ALIAS)?.map { base64(it.encoded) }.orEmpty()
      mapOf(
        "keyId" to ALIAS,
        "publicKey" to base64(uncompressedPoint(publicKey)),
        "certificateChain" to chain
      )
    }

    AsyncFunction("signWithHardwareKey") { _: String, payload: String -> sign(payload) }

    AsyncFunction("createSoftwareKey") { base64(uncompressedPoint(generateKey(null))) }

    AsyncFunction("signWithSoftwareKey") { payload: String -> sign(payload) }

    AsyncFunction("deleteKeys") {
      keyStore().deleteEntry(ALIAS)
    }
  }
}

private fun keyStore(): KeyStore = KeyStore.getInstance(KEYSTORE).apply { load(null) }

private fun sha256(value: String): ByteArray =
  MessageDigest.getInstance("SHA-256").digest(value.toByteArray(Charsets.UTF_8))

private fun base64(bytes: ByteArray): String = Base64.encodeToString(bytes, Base64.NO_WRAP)

private fun generateKey(challenge: ByteArray?): ECPublicKey {
  keyStore().deleteEntry(ALIAS)
  val strongBox = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P
  return try {
    generateKey(challenge, strongBox)
  } catch (_: StrongBoxUnavailableException) {
    generateKey(challenge, false)
  }
}

private fun generateKey(challenge: ByteArray?, strongBox: Boolean): ECPublicKey {
  val spec = KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_SIGN)
    .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
    .setDigests(KeyProperties.DIGEST_SHA256)
    .apply {
      if (challenge != null) setAttestationChallenge(challenge)
      if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) setIsStrongBoxBacked(true)
    }
    .build()
  val generator = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, KEYSTORE)
  generator.initialize(spec)
  return generator.generateKeyPair().public as ECPublicKey
}

private fun sign(payload: String): String {
  val key = keyStore().getKey(ALIAS, null) as? PrivateKey
    ?: error("No device key exists; create one first.")
  return Signature.getInstance("SHA256withECDSA").run {
    initSign(key)
    update(payload.toByteArray(Charsets.UTF_8))
    base64(sign())
  }
}

private fun uncompressedPoint(key: ECPublicKey): ByteArray =
  byteArrayOf(0x04) + coordinate(key.w.affineX) + coordinate(key.w.affineY)

private fun coordinate(value: BigInteger): ByteArray {
  val bytes = value.toByteArray().dropWhile { it == 0.toByte() }.toByteArray()
  return ByteArray(32 - bytes.size) + bytes
}
