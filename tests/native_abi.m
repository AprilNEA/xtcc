// This fixture only encodes in-memory data; it allocates no ports and attaches to no processes.
#import <Foundation/Foundation.h>
#import "../native/ControlProtocol.h"
#include <stdio.h>

int main(void) {
    @autoreleasepool {
        NSData *data = TCEncode(@{@"ok": @YES});
        if (!data)
            return 1;
        TCFrame frame;
        TCPrepare(&frame, TCStatus, UINT64_C(0x1020304050607080), data.bytes,
                  (uint32_t)data.length);
        printf("{\"header\":%zu,\"frame\":%zu,\"align\":%zu,"
               "\"request_offset\":%zu,\"body_offset\":%zu,"
               "\"audit_trailer\":%zu,\"audit_offset\":%zu,"
               "\"max_trailer\":%zu,\"death_size\":%zu,"
               "\"death_port_offset\":%zu,\"service\":\"%s\",\"wire\":\"",
               sizeof(mach_msg_header_t), sizeof(TCFrame), _Alignof(TCFrame),
               offsetof(TCFrame, request), offsetof(TCFrame, body),
               sizeof(mach_msg_audit_trailer_t), offsetof(mach_msg_audit_trailer_t, msgh_audit),
               sizeof(mach_msg_max_trailer_t), offsetof(mach_dead_name_notification_t, trailer),
               offsetof(mach_dead_name_notification_t, not_port), TC_SERVICE);
        const unsigned char *bytes = (const unsigned char *)&frame;
        for (size_t i = 0; i < frame.header.msgh_size; ++i)
            printf("%02x", bytes[i]);
        puts("\"}");
    }
    return 0;
}
