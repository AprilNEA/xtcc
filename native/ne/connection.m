#import "connection.h"
#import <SystemConfiguration/SystemConfiguration.h>

static NSString *StateName(SCNetworkConnectionStatus status) {
    switch (status) {
    case kSCNetworkConnectionDisconnected:
        return @"disconnected";
    case kSCNetworkConnectionConnecting:
        return @"connecting";
    case kSCNetworkConnectionConnected:
        return @"connected";
    case kSCNetworkConnectionDisconnecting:
        return @"disconnecting";
    default:
        return @"invalid";
    }
}

static NSDictionary *ConnectionError(NSString *operation) {
    return @{
        @"ok" : @NO,
        @"error" : [NSString stringWithFormat:@"%@: %s", operation, SCErrorString(SCError())]
    };
}

static void Changed(SCNetworkConnectionRef connection, SCNetworkConnectionStatus status,
                    void *info) {
    (void)connection;
    SCNetworkConnectionStatus target = *(SCNetworkConnectionStatus *)info;
    if (status == target || status == kSCNetworkConnectionInvalid ||
        (target == kSCNetworkConnectionConnected && status == kSCNetworkConnectionDisconnected)) {
        CFRunLoopStop(CFRunLoopGetCurrent());
    }
}

NSDictionary *XTCCConnection(NSString *identifier, NSString *operation) {
    BOOL readOnly = [operation isEqualToString:@"status"];
    SCNetworkConnectionStatus target = [operation isEqualToString:@"connect"]
                                           ? kSCNetworkConnectionConnected
                                           : kSCNetworkConnectionDisconnected;
    SCNetworkConnectionContext context = {.info = &target};
    SCNetworkConnectionRef connection = SCNetworkConnectionCreateWithServiceID(
        kCFAllocatorDefault, (__bridge CFStringRef)identifier, Changed, &context);
    if (!connection)
        return ConnectionError(@"Open VPN connection");
    SCNetworkConnectionStatus status = SCNetworkConnectionGetStatus(connection);
    NSDictionary *failure = nil;
    if (status == kSCNetworkConnectionInvalid) {
        failure = ConnectionError(@"Read VPN status");
    } else if (!readOnly && status != target) {
        if (!SCNetworkConnectionScheduleWithRunLoop(connection, CFRunLoopGetCurrent(),
                                                    kCFRunLoopDefaultMode)) {
            failure = ConnectionError(@"Observe VPN status");
        } else {
            BOOL accepted = target == kSCNetworkConnectionConnected
                                ? SCNetworkConnectionStart(connection, NULL, true)
                                : SCNetworkConnectionStop(connection, true);
            if (!accepted) {
                failure = ConnectionError(operation);
            } else {
                if (SCNetworkConnectionGetStatus(connection) != target)
                    CFRunLoopRun();
                status = SCNetworkConnectionGetStatus(connection);
            }
            if (!SCNetworkConnectionUnscheduleFromRunLoop(connection, CFRunLoopGetCurrent(),
                                                          kCFRunLoopDefaultMode) &&
                !failure)
                failure = ConnectionError(@"Stop observing VPN status");
        }
    }
    CFRelease(connection);
    if (failure)
        return failure;
    if (!readOnly && status != target) {
        return @{
            @"ok" : @NO,
            @"state" : StateName(status),
            @"error" : [NSString
                stringWithFormat:@"VPN did not become %@ (state: %@); inspect provider logs",
                                 StateName(target), StateName(status)]
        };
    }
    return @{@"ok" : @YES, @"state" : StateName(status)};
}
