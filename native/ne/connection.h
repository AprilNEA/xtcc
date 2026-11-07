#import <Foundation/Foundation.h>

/// Query a VPN or wait for the requested connection transition using SystemConfiguration.
NSDictionary *XTCCConnection(NSString *identifier, NSString *operation);
