import CryptoKit
import DeviceCheck
import ExpoModulesCore
import Foundation
import Security

private let softwareKeyService = "onicode.push.device-key"

public class OniDeviceAttestationModule: Module {
  public func definition() -> ModuleDefinition {
    Name("OniDeviceAttestation")

    Function("isHardwareAttestationSupported") { () -> Bool in
      DCAppAttestService.shared.isSupported
    }

    AsyncFunction("attestHardwareKey") { (challenge: String) async throws -> [String: Any] in
      let service = DCAppAttestService.shared
      let keyId = try await service.generateKey()
      let attestation = try await service.attestKey(keyId, clientDataHash: sha256(challenge))
      return ["keyId": keyId, "attestation": attestation.base64EncodedString()]
    }

    AsyncFunction("signWithHardwareKey") { (keyId: String, payload: String) async throws -> String in
      try await DCAppAttestService.shared
        .generateAssertion(keyId, clientDataHash: sha256(payload))
        .base64EncodedString()
    }

    AsyncFunction("createSoftwareKey") { () throws -> String in
      let key = P256.Signing.PrivateKey()
      try storeSoftwareKey(key.rawRepresentation)
      return key.publicKey.x963Representation.base64EncodedString()
    }

    AsyncFunction("signWithSoftwareKey") { (payload: String) throws -> String in
      guard let raw = try loadSoftwareKey() else { throw MissingSoftwareKeyException() }
      let key = try P256.Signing.PrivateKey(rawRepresentation: raw)
      return try key.signature(for: Data(payload.utf8)).derRepresentation.base64EncodedString()
    }

    AsyncFunction("deleteKeys") { () throws in
      try deleteSoftwareKey()
    }
  }
}

private func sha256(_ value: String) -> Data {
  Data(SHA256.hash(data: Data(value.utf8)))
}

private func keyQuery() -> [String: Any] {
  [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: softwareKeyService,
    kSecAttrAccount as String: "p256",
  ]
}

private func storeSoftwareKey(_ raw: Data) throws {
  try deleteSoftwareKey()
  var query = keyQuery()
  query[kSecValueData as String] = raw
  // Background pushes refresh grants while the device is locked.
  query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
  let status = SecItemAdd(query as CFDictionary, nil)
  guard status == errSecSuccess else { throw KeychainException(status) }
}

private func loadSoftwareKey() throws -> Data? {
  var query = keyQuery()
  query[kSecReturnData as String] = true
  var item: CFTypeRef?
  let status = SecItemCopyMatching(query as CFDictionary, &item)
  if status == errSecItemNotFound { return nil }
  guard status == errSecSuccess else { throw KeychainException(status) }
  return item as? Data
}

private func deleteSoftwareKey() throws {
  let status = SecItemDelete(keyQuery() as CFDictionary)
  guard status == errSecSuccess || status == errSecItemNotFound else { throw KeychainException(status) }
}

private final class MissingSoftwareKeyException: Exception, @unchecked Sendable {
  override var reason: String { "No device key exists; create one first." }
}

private final class KeychainException: GenericException<OSStatus>, @unchecked Sendable {
  override var reason: String { "Keychain operation failed with status \(param)." }
}
