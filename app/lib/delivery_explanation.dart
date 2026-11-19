/// Pure presentation helpers mapping typed Rust delivery states and `RouteReason`
/// codes into stable, honest user-facing explanations.
class DeliveryExplanation {
  const DeliveryExplanation._();

  static const int preset2mSecs = 120;
  static const int preset15mSecs = 900;
  static const int preset1hSecs = 3600;
  static const int privacyMaxDeadlineSecs = 3600;
  static const int normalMaxDeadlineSecs = 7 * 86400;

  static String formatTransport(String raw) {
    switch (raw) {
      case 'DirectBle':
        return 'Direct BLE';
      case 'BleMeshFlood':
        return 'BLE Mesh';
      case 'IrohDirect':
        return 'Iroh Direct';
      case 'IrohRelay':
        return 'Iroh Relay';
      default:
        return 'Unknown';
    }
  }

  static String formatDeadlineLabel(int? deadlineSecs) {
    if (deadlineSecs == null) return 'Default';
    switch (deadlineSecs) {
      case preset2mSecs:
        return '2m';
      case preset15mSecs:
        return '15m';
      case preset1hSecs:
        return '1h';
      default:
        return '${deadlineSecs}s';
    }
  }

  static bool isDeadlineCompatibleWithMode(
    int? deadlineSecs, {
    required bool isPrivacy,
  }) {
    if (deadlineSecs == null) return true;
    final maxSecs = isPrivacy ? privacyMaxDeadlineSecs : normalMaxDeadlineSecs;
    return deadlineSecs >= 1 && deadlineSecs <= maxSecs;
  }

  static String formatRemainingDeadline(int expiresAtUnix, {DateTime? now}) {
    if (expiresAtUnix <= 0) return '';
    final nowSecs = (now ?? DateTime.now()).millisecondsSinceEpoch ~/ 1000;
    final rem = expiresAtUnix - nowSecs;
    if (rem <= 0) return 'Expired';
    if (rem < 60) return '${rem}s left';
    if (rem < 3600) return '${(rem / 60).ceil()}m left';
    if (rem < 86400) return '${(rem / 3600).floor()}h ${(rem % 3600) ~/ 60}m left';
    return '${(rem / 86400).floor()}d left';
  }

  static String explain({
    required String state,
    required String reason,
    String transport = 'Unknown',
    int attemptCount = 0,
  }) {
    switch (state) {
      case 'Delivered':
        final tr = formatTransport(transport);
        return tr == 'Unknown'
            ? 'Delivered (acknowledged).'
            : 'Delivered via $tr (acknowledged).';
      case 'Expired':
        return 'Expired before confirmed delivery.';
      case 'Cancelled':
        return 'Cancelled before delivery.';
      case 'Failed':
        return 'Stopped after retry limit.';
      default:
        break;
    }

    switch (reason) {
      case 'WaitingForApprovedGateway':
        return 'Waiting for an approved gateway.';
      case 'SessionHandshakeRequired':
        return 'Establishing a secure session.';
      case 'RetryingAlternateGateway':
        return 'Retrying through another gateway.';
      case 'ResourcePolicyPaused':
        return 'Delivery paused by resource policy.';
      case 'PrivacyRelayRestricted':
        return 'Privacy mode blocks relay gateways; waiting for direct route.';
      case 'GatewayOptedOut':
        return 'Gateway off; waiting for local BLE.';
      case 'ExhaustedAttemptsQueue':
        return 'Retry limit reached; backing off in queue.';
      default:
        if (state == 'InFlight' || attemptCount > 0 && state != 'Queued') {
          return 'Waiting for recipient acknowledgment.';
        }
        return 'Waiting for recipient acknowledgment or route.';
    }
  }
}
