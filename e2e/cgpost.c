// cgpost — post REAL input events at the HID tap, for verifying the
// platform seam the in-process e2e harness enters below (real NSEvent
// delivery, IME routing, capture pairing — everything synthesis skips).
//
//   cc -O2 -framework ApplicationServices -o cgpost cgpost.c
//   cgpost click X Y        one left click at screen points
//   cgpost move X Y         move the pointer
//   cgpost key CODE         press+release a virtual key code
//                           (36 return · 48 tab · 38 j · 40 k · 125 down ·
//                            126 up · 44 slash)
//
// Needs Accessibility permission on the invoking terminal. Aim it at a
// scratch instance (`superapp --db /tmp/x.db --window …`), never at the
// live session.
#include <ApplicationServices/ApplicationServices.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    if (!strcmp(argv[1], "click") && argc >= 4) {
        CGPoint p = CGPointMake(atof(argv[2]), atof(argv[3]));
        CGEventRef m = CGEventCreateMouseEvent(NULL, kCGEventMouseMoved, p, kCGMouseButtonLeft);
        CGEventPost(kCGHIDEventTap, m);
        CFRelease(m);
        usleep(50000);
        CGEventRef d = CGEventCreateMouseEvent(NULL, kCGEventLeftMouseDown, p, kCGMouseButtonLeft);
        CGEventRef u = CGEventCreateMouseEvent(NULL, kCGEventLeftMouseUp, p, kCGMouseButtonLeft);
        CGEventPost(kCGHIDEventTap, d);
        usleep(60000);
        CGEventPost(kCGHIDEventTap, u);
        CFRelease(d);
        CFRelease(u);
    } else if (!strcmp(argv[1], "move") && argc >= 4) {
        CGPoint p = CGPointMake(atof(argv[2]), atof(argv[3]));
        CGEventRef m = CGEventCreateMouseEvent(NULL, kCGEventMouseMoved, p, kCGMouseButtonLeft);
        CGEventPost(kCGHIDEventTap, m);
        CFRelease(m);
    } else if (!strcmp(argv[1], "key") && argc >= 3) {
        CGKeyCode c = (CGKeyCode)atoi(argv[2]);
        CGEventRef d = CGEventCreateKeyboardEvent(NULL, c, true);
        CGEventRef u = CGEventCreateKeyboardEvent(NULL, c, false);
        CGEventPost(kCGHIDEventTap, d);
        usleep(40000);
        CGEventPost(kCGHIDEventTap, u);
        CFRelease(d);
        CFRelease(u);
    } else {
        return 2;
    }
    return 0;
}
