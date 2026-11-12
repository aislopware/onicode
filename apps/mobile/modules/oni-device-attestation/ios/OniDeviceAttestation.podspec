Pod::Spec.new do |s|
  s.name = 'OniDeviceAttestation'
  s.version = '1.0.0'
  s.summary = 'App Attest and device keys for the Onicode push gateway.'
  s.author = 'Onicode Tools'
  s.homepage = 'https://onicode.com'
  s.platform = :ios, '18.0'
  s.source = { :path => '.' }
  s.static_framework = true
  s.dependency 'ExpoModulesCore'
  s.source_files = '*.swift'
  s.frameworks = 'CryptoKit', 'DeviceCheck', 'Security'
end
