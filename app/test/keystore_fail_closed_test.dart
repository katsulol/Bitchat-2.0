import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:bitchat2/mesh_bridge.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const channel = MethodChannel('com.bitchat.mesh/ble');

  tearDown(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, null);
  });

  test('startMesh fails closed on Android Keystore PlatformException', () async {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, (MethodCall call) async {
      if (call.method == 'getAppStoragePath') {
        return '.';
      }
      if (call.method == 'getPlatformDek') {
        throw PlatformException(
          code: 'KEYSTORE_LOCKED',
          message: 'Encrypted outbox exists without KEK',
        );
      }
      return null;
    });

    final bridge = MeshBridge();
    addTearDown(bridge.dispose);

    await bridge.startMesh();

    expect(bridge.hasKeystoreError, isTrue);
    expect(bridge.lastKeystoreError, contains('KEYSTORE_LOCKED'));
    expect(bridge.myShortId, 'Initializing...');
  });
}
