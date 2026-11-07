#import <Foundation/Foundation.h>
#import <NetworkExtension/NetworkExtension.h>
#import <Security/Security.h>
#import <SystemExtensions/SystemExtensions.h>
#import "ne/connection.h"
#include <unistd.h>

@protocol XTCCExtension <NSObject>
@property(nonatomic) BOOL enabled;
@property(nonatomic, readonly) BOOL active;
@property(nonatomic, readonly) BOOL rebootRequired;
@property(nonatomic, readonly) NSString *identifier;
@property(nonatomic, readonly) NSString *stateString;
@property(nonatomic, readonly) NSString *teamID;
@property(nonatomic, readonly) NSURL *containingAppURL;
@property(nonatomic, readonly) NSURL *stagedBundleURL;
@end

@protocol XTCCExtensionClient <NSObject>
- (NSArray *)listAllExtensionsWithCategory:(NSString *)category error:(NSError **)error;
- (BOOL)updateExtensionWithInfo:(id)info error:(NSError **)error;
@end

@protocol XTCCConfiguration <NSObject>
- (id)initWithName:(NSString *)name grade:(NSInteger)grade;
@property(nonatomic) NSString *application;
@property(nonatomic) NSString *applicationIdentifier;
@property(nonatomic) NSString *applicationName;
@property(nonatomic) id contentFilter;
@property(nonatomic, readonly) NSString *name;
@property(nonatomic, readonly) NSUUID *identifier;
@property(nonatomic, readonly, getter=isEnabled) BOOL enabled;
@end

@protocol XTCCContentFilter <NSObject>
@property(nonatomic, getter=isEnabled) BOOL enabled;
@property(nonatomic) NEFilterProviderConfiguration *provider;
@end

@protocol XTCCProviderProtocol <NSObject>
@property(nonatomic) NSString *pluginType;
@property(nonatomic) NSString *providerBundleIdentifier;
@property(nonatomic) NSString *designatedRequirement;
@property(nonatomic) NSDictionary *providerConfiguration;
@property(nonatomic) NSString *serverAddress;
@end

@protocol XTCCProxy <NSObject>
@property(nonatomic, getter=isEnabled) BOOL enabled;
@property(nonatomic) id<XTCCProviderProtocol> protocol;
@end

@protocol XTCCConfigurationManager <NSObject>
- (void)loadConfigurationsWithCompletionQueue:(dispatch_queue_t)queue
                                      handler:(void (^)(NSArray *, NSError *))handler;
- (void)saveConfiguration:(id)configuration
      withCompletionQueue:(dispatch_queue_t)queue
                  handler:(void (^)(NSError *))handler;
- (void)removeConfiguration:(id)configuration
        withCompletionQueue:(dispatch_queue_t)queue
                    handler:(void (^)(NSError *))handler;
@end

_Noreturn static void Finish(NSDictionary *record) {
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:record options:0 error:&error];
    if (!data) {
        fprintf(stderr, "Cannot encode NE response: %s\n", error.description.UTF8String);
        exit(1);
    }
    if (fwrite(data.bytes, 1, data.length, stdout) != data.length || putchar('\n') == EOF) {
        exit(1);
    }
    exit([record[@"ok"] boolValue] ? 0 : 1);
}

_Noreturn static void Fail(NSString *message) {
    Finish(@{@"ok" : @NO, @"error" : message});
}

static Class RequireClass(NSString *name) {
    Class cls = NSClassFromString(name);
    if (!cls)
        Fail([NSString stringWithFormat:@"Unsupported macOS private API: %@", name]);
    return cls;
}

static NSDictionary *ExtensionSummary(id<XTCCExtension> extension) {
    return @{
        @"identifier" : extension.identifier,
        @"enabled" : @(extension.enabled),
        @"active" : @(extension.active),
        @"state" : extension.stateString ?: @"",
        @"team_id" : extension.teamID ?: (id)[NSNull null],
        @"reboot_required" : @(extension.rebootRequired),
        @"application_path" : extension.containingAppURL.path ?: (id)[NSNull null],
        @"bundle_path" : extension.stagedBundleURL.path ?: (id)[NSNull null]
    };
}

static void Extensions(NSDictionary *request) {
    id<XTCCExtensionClient> client = [RequireClass(@"OSSystemExtensionClient") new];
    NSError *error = nil;
    NSString *category = @"com.apple.system_extension.network_extension";
    NSArray *extensions = [client listAllExtensionsWithCategory:category error:&error];
    if (!extensions || error)
        Fail([NSString stringWithFormat:@"List NE extensions: %@", error]);
    NSString *operation = request[@"operation"];
    if ([operation isEqualToString:@"extensions"]) {
        NSMutableArray *records = [NSMutableArray array];
        for (id<XTCCExtension> extension in extensions) {
            [records addObject:ExtensionSummary(extension)];
        }
        Finish(@{@"ok" : @YES, @"extensions" : records});
    }
    NSString *identifier = request[@"identifier"];
    NSMutableArray *matches = [NSMutableArray array];
    for (id<XTCCExtension> extension in extensions) {
        if ([extension.identifier isEqualToString:identifier])
            [matches addObject:extension];
    }
    if (matches.count != 1) {
        Fail([NSString
            stringWithFormat:@"Expected one registered NE extension for %@; found %lu. "
                             @"Activate the extension from its containing application first.",
                             identifier, (unsigned long)matches.count]);
    }
    id<XTCCExtension> extension = matches.firstObject;
    if (extension.enabled) {
        Finish(@{@"ok" : @YES, @"extension" : ExtensionSummary(extension)});
    }
    extension.enabled = YES;
    if (![client updateExtensionWithInfo:extension error:&error] || error) {
        Fail([NSString stringWithFormat:@"Approve %@: %@", identifier, error]);
    }
    NSArray *updated = [client listAllExtensionsWithCategory:category error:&error];
    if (!updated || error)
        Fail([NSString stringWithFormat:@"Read approval result: %@", error]);
    for (id<XTCCExtension> current in updated) {
        if ([current.identifier isEqualToString:identifier]) {
            if (!current.enabled)
                Fail(@"System extension approval was not retained");
            Finish(@{@"ok" : @YES, @"extension" : ExtensionSummary(current)});
        }
    }
    Fail(@"Approved extension disappeared; inspect ne list before retrying");
}

static NSDictionary *Signature(NSURL *url) {
    SecStaticCodeRef code = NULL;
    SecRequirementRef requirement = NULL;
    CFStringRef text = NULL;
    CFDictionaryRef information = NULL;
    OSStatus status =
        SecStaticCodeCreateWithPath((__bridge CFURLRef)url, kSecCSDefaultFlags, &code);
    if (status == errSecSuccess) {
        status = SecStaticCodeCheckValidity(code, kSecCSStrictValidate, NULL);
    }
    if (status == errSecSuccess) {
        status =
            SecCodeCopyDesignatedRequirement((SecCodeRef)code, kSecCSDefaultFlags, &requirement);
    }
    if (status == errSecSuccess) {
        status = SecRequirementCopyString(requirement, kSecCSDefaultFlags, &text);
    }
    if (status == errSecSuccess) {
        status =
            SecCodeCopySigningInformation((SecCodeRef)code, kSecCSSigningInformation, &information);
    }
    if (code)
        CFRelease(code);
    if (requirement)
        CFRelease(requirement);
    NSString *requirementString = CFBridgingRelease(text);
    NSDictionary *signingInformation = CFBridgingRelease(information);
    if (status != errSecSuccess) {
        NSString *message = CFBridgingRelease(SecCopyErrorMessageString(status, NULL));
        Fail([NSString
            stringWithFormat:@"Check signature of %@: %@ (%d)", url.path, message, (int)status]);
    }
    return @{
        @"requirement" : requirementString,
        @"entitlements" : signingInformation[(__bridge NSString *)kSecCodeInfoEntitlementsDict]
            ?: @{}
    };
}

static NSString *ComponentKey(id<XTCCConfiguration> configuration) {
    for (NSString *key in @[ @"contentFilter", @"dnsProxy", @"VPN", @"appVPN" ]) {
        if ([(id)configuration valueForKey:key])
            return key;
    }
    return nil;
}

static NSDictionary *ConfigurationSummary(id<XTCCConfiguration> configuration) {
    return @{
        @"identifier" : configuration.identifier.UUIDString,
        @"name" : configuration.name ?: @"",
        @"application" : configuration.application ?: (id)[NSNull null],
        @"kind" : ComponentKey(configuration) ?: @"unsupported",
        @"enabled" : @(configuration.enabled)
    };
}

static id<XTCCConfiguration> ProviderConfiguration(NSDictionary *request, NSArray *configurations) {
    NSString *operation = request[@"operation"];
    BOOL isFilter = [operation isEqualToString:@"filter"];
    BOOL isVPN = [operation isEqualToString:@"vpn"];
    NSString *key = isFilter ? @"contentFilter" : isVPN ? @"VPN" : @"dnsProxy";
    NSDictionary *extension = request[@"extension"];
    NSString *identifier = extension[@"identifier"];
    NSURL *providerURL = [NSURL fileURLWithPath:extension[@"bundle_path"]];
    NSURL *applicationURL = [NSURL fileURLWithPath:extension[@"application_path"]];
    NSBundle *bundle = [NSBundle bundleWithURL:providerURL];
    NSBundle *application = [NSBundle bundleWithURL:applicationURL];
    if (![bundle.bundleIdentifier isEqualToString:identifier] || !application.bundleIdentifier) {
        Fail(@"Registered provider or containing application is missing or has changed identity");
    }
    NSDictionary *classes = bundle.infoDictionary[@"NetworkExtension"][@"NEProviderClasses"];
    BOOL sockets = classes[@"com.apple.networkextension.filter-data"] != nil;
    BOOL packets = classes[@"com.apple.networkextension.filter-packet"] != nil;
    BOOL matchesKind = isFilter ? (sockets || packets)
                                : classes[isVPN ? @"com.apple.networkextension.packet-tunnel"
                                                : @"com.apple.networkextension.dns-proxy"] != nil;
    if (!matchesKind) {
        Fail([NSString
            stringWithFormat:@"Provider does not support %@; no configuration was changed",
                             operation]);
    }
    NSDictionary *providerSignature = Signature(providerURL);
    NSDictionary *applicationSignature = Signature(applicationURL);
    NSDictionary *vendor = nil;
    if (request[@"vendor_configuration"] != [NSNull null]) {
        NSError *error = nil;
        NSData *data = [NSData dataWithContentsOfFile:request[@"vendor_configuration"]
                                              options:0
                                                error:&error];
        if (!data)
            Fail([NSString stringWithFormat:@"Read vendor configuration: %@", error]);
        vendor = [NSPropertyListSerialization propertyListWithData:data
                                                           options:NSPropertyListImmutable
                                                            format:NULL
                                                             error:&error];
        if (![vendor isKindOfClass:[NSDictionary class]]) {
            Fail([NSString stringWithFormat:@"Vendor configuration must be a plist dictionary: %@",
                                            error ?: @"wrong root type"]);
        }
    }
    id<XTCCConfiguration> selected = nil;
    for (id<XTCCConfiguration> configuration in configurations) {
        id component = [(id)configuration valueForKey:key];
        if (![configuration.application isEqualToString:application.bundleIdentifier] || !component)
            continue;
        BOOL matchesProvider;
        if (isFilter) {
            NEFilterProviderConfiguration *provider = ((id<XTCCContentFilter>)component).provider;
            matchesProvider =
                [provider.filterDataProviderBundleIdentifier isEqualToString:identifier] ||
                [provider.filterPacketProviderBundleIdentifier isEqualToString:identifier];
        } else {
            id provider = ((id<XTCCProxy>)component).protocol;
            matchesProvider = [provider respondsToSelector:@selector(providerBundleIdentifier)] &&
                              [[provider providerBundleIdentifier] isEqualToString:identifier];
        }
        if (!matchesProvider) {
            if (isVPN)
                continue;
            Fail(@"This application already has a configuration for a different provider; no "
                 @"configuration was changed");
        }
        if (selected)
            Fail(@"Multiple configurations match this provider; manage them by configuration UUID");
        selected = configuration;
    }
    if (!selected) {
        NSString *name = application.infoDictionary[@"CFBundleDisplayName"] ?:
                         application.infoDictionary[@"CFBundleName"] ?: application.bundleIdentifier;
        selected =
            [(id<XTCCConfiguration>)[RequireClass(@"NEConfiguration") alloc] initWithName:name
                                                                                    grade:1];
        selected.application = application.bundleIdentifier;
        selected.applicationIdentifier =
            applicationSignature[@"entitlements"][@"com.apple.application-identifier"]
                ?: application.bundleIdentifier;
        selected.applicationName = name;
        if (isFilter) {
            id<XTCCContentFilter> filter = [RequireClass(@"NEContentFilter") new];
            filter.provider = [NEFilterProviderConfiguration new];
            selected.contentFilter = filter;
        } else {
            id<XTCCProxy> component = [RequireClass(isVPN ? @"NEVPN" : @"NEDNSProxy") new];
            component.protocol =
                isVPN ? (id)[NETunnelProviderProtocol new] : (id)[NEDNSProxyProviderProtocol new];
            [(id)selected setValue:component forKey:key];
        }
    }
    if (!isFilter) {
        id<XTCCProxy> component = [(id)selected valueForKey:key];
        id<XTCCProviderProtocol> provider = component.protocol;
        provider.pluginType = application.bundleIdentifier;
        provider.providerBundleIdentifier = identifier;
        provider.designatedRequirement = providerSignature[@"requirement"];
        if (vendor)
            provider.providerConfiguration = vendor;
        if (isVPN)
            provider.serverAddress = request[@"server_address"];
        component.enabled = YES;
        return selected;
    }
    NEFilterProviderConfiguration *provider =
        ((id<XTCCContentFilter>)selected.contentFilter).provider;
    [provider setValue:application.bundleIdentifier forKey:@"pluginType"];
    if (sockets) {
        provider.filterSockets = YES;
        provider.filterDataProviderBundleIdentifier = identifier;
        [provider setValue:providerSignature[@"requirement"]
                    forKey:@"dataProviderDesignatedRequirement"];
        [provider setValue:providerURL forKey:@"dataProviderURL"];
    }
    if (packets) {
        provider.filterPackets = YES;
        provider.filterPacketProviderBundleIdentifier = identifier;
        [provider setValue:providerSignature[@"requirement"]
                    forKey:@"packetProviderDesignatedRequirement"];
        [provider setValue:providerURL forKey:@"packetProviderURL"];
    }
    if (vendor)
        provider.vendorConfiguration = vendor;
    ((id<XTCCContentFilter>)selected.contentFilter).enabled = YES;
    return selected;
}

static void Configurations(NSDictionary *request) {
    id<XTCCConfigurationManager> manager = [RequireClass(@"NEConfigurationManager") new];
    void (^loaded)(NSArray *, NSError *) = ^(NSArray *configurations, NSError *error) {
        @try {
            if (!configurations || error)
                Fail([NSString stringWithFormat:@"Load NE configurations: %@", error]);
            NSString *operation = request[@"operation"];
            if ([operation isEqualToString:@"configurations"]) {
                NSMutableArray *records = [NSMutableArray array];
                for (id<XTCCConfiguration> configuration in configurations) {
                    [records addObject:ConfigurationSummary(configuration)];
                }
                Finish(@{@"ok" : @YES, @"configurations" : records});
            }
            id<XTCCConfiguration> selected = nil;
            if ([@[ @"filter", @"dns-proxy", @"vpn" ] containsObject:operation]) {
                selected = ProviderConfiguration(request, configurations);
            } else {
                NSUUID *identifier = [[NSUUID alloc] initWithUUIDString:request[@"identifier"]];
                if (!identifier)
                    Fail(@"Configuration identifier must be a UUID from ne list");
                for (id<XTCCConfiguration> configuration in configurations) {
                    if ([configuration.identifier isEqual:identifier])
                        selected = configuration;
                }
                if (!selected)
                    Fail(@"Configuration not found; no configuration was changed");
                NSString *key = ComponentKey(selected);
                if (!key)
                    Fail(@"Unsupported configuration type; no configuration was changed");
                if ([@[ @"connect", @"disconnect", @"status" ] containsObject:operation]) {
                    if (![key isEqualToString:@"VPN"])
                        Fail(@"Connection control requires a VPN configuration UUID");
                    if ([operation isEqualToString:@"connect"] && !selected.enabled)
                        Fail(@"VPN configuration is disabled; run ne enable first");
                    NSMutableDictionary *result =
                        [XTCCConnection(identifier.UUIDString, operation) mutableCopy];
                    result[@"configuration"] = ConfigurationSummary(selected);
                    result[@"operation"] = operation;
                    Finish(result);
                }
                if (![operation isEqualToString:@"remove"]) {
                    [[(id)selected valueForKey:key]
                        setEnabled:[operation isEqualToString:@"enable"]];
                }
            }
            NSDictionary *record = ConfigurationSummary(selected);
            void (^completion)(NSError *) = ^(NSError *saveError) {
                // NEConfigurationManager reports an identical saved configuration as error 9.
                BOOL unchanged = ![operation isEqualToString:@"remove"] &&
                                 [saveError.domain isEqualToString:@"NEConfigurationErrorDomain"] &&
                                 saveError.code == 9;
                if (saveError && !unchanged)
                    Fail([NSString
                        stringWithFormat:@"%@ NE configuration: %@", operation, saveError]);
                Finish(@{
                    @"ok" : @YES,
                    @"operation" : operation,
                    @"configuration" : record,
                    @"unchanged" : @(unchanged)
                });
            };
            if ([operation isEqualToString:@"remove"]) {
                [manager removeConfiguration:selected
                         withCompletionQueue:dispatch_get_main_queue()
                                     handler:completion];
            } else {
                [manager saveConfiguration:selected
                       withCompletionQueue:dispatch_get_main_queue()
                                   handler:completion];
            }
        } @catch (NSException *exception) {
            Fail([NSString stringWithFormat:@"NE private API failed: %@: %@", exception.name,
                                            exception.reason]);
        }
    };
    [manager loadConfigurationsWithCompletionQueue:dispatch_get_main_queue() handler:loaded];
    [[NSRunLoop currentRunLoop] run];
    Fail(@"NE run loop returned without a result");
}

// This constructor must run only in a new short-lived client, never in a live system daemon.
__attribute__((constructor)) static void Run(void) {
    if (!getenv("XTCC_NE_ONESHOT"))
        return;
    @autoreleasepool {
        @try {
            if (geteuid() != 0)
                Fail(@"root_required");
            NSError *error = nil;
            NSData *data = [[NSFileHandle fileHandleWithStandardInput]
                readDataToEndOfFileAndReturnError:&error];
            if (!data)
                Fail([NSString stringWithFormat:@"Read NE request: %@", error]);
            NSDictionary *request = [NSJSONSerialization JSONObjectWithData:data
                                                                    options:0
                                                                      error:&error];
            if (![request isKindOfClass:[NSDictionary class]])
                Fail(@"NE request must be a JSON object");
            NSString *operation = request[@"operation"];
            if ([operation isEqualToString:@"extensions"] ||
                [operation isEqualToString:@"approve"]) {
                Extensions(request);
            } else if ([@[
                           @"configurations", @"filter", @"dns-proxy", @"vpn", @"connect",
                           @"disconnect", @"status", @"enable", @"disable", @"remove"
                       ] containsObject:operation]) {
                Configurations(request);
            } else {
                Fail(@"Unknown NE operation");
            }
        } @catch (NSException *exception) {
            Fail([NSString stringWithFormat:@"NE private API failed: %@: %@", exception.name,
                                            exception.reason]);
        }
    }
}
