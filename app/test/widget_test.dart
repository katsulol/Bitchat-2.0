import 'package:flutter_test/flutter_test.dart';
import 'package:bitchat2/main.dart';

void main() {
  testWidgets('BitchatApp smoke test', (WidgetTester tester) async {
    await tester.pumpWidget(const BitchatApp());
    expect(find.text('Bitchat 2.0 Mesh'), findsOneWidget);
  });
}
