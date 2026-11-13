/* A display made to the size of the client's window, through CoreGraphics' CGVirtualDisplay, which is not public
   but is what display tools use. Once its mode has been chosen, it keeps its settings and stays as long as the
   process that made it, so a process of its own makes it, and another one at another size (see
   virtual_screen.rs). */
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(retain, nonatomic) NSString *name;
@property(nonatomic) unsigned int maxPixelsWide, maxPixelsHigh, vendorID, productID, serialNum;
@property(nonatomic) CGSize sizeInMillimeters;
@end

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)rate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray *modes;
@property(nonatomic) unsigned int hiDPI;
@end

@interface CGVirtualDisplay : NSObject
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@property(readonly, nonatomic) CGDirectDisplayID displayID;
@end

/* The largest a client's window makes it, in pixels (as screen.rs has it), and its size as a monitor would
   state it, which only sets what macOS takes its density to be. */
#define LARGEST 7680
#define MILLIMETRES_WIDE 600
#define MILLIMETRES_HIGH 340
/* How long the display's modes may take to change after new settings: tries, and microseconds between them. */
#define MODE_TRIES 40
#define MODE_WAIT 50000

static bool is_mode(CGDisplayModeRef mode, unsigned width, unsigned height, unsigned scale)
{
    return mode && CGDisplayModeGetWidth(mode) == width && CGDisplayModeGetHeight(mode) == height
        && CGDisplayModeGetPixelWidth(mode) == width * scale;
}

/* Whether `display` is at `width` x `height` points at `scale` pixels a point, else switches it to that mode if
   it has it. */
static bool in_mode(CGDirectDisplayID display, unsigned width, unsigned height, unsigned scale)
{
    CGDisplayModeRef current = CGDisplayCopyDisplayMode(display);
    bool there = is_mode(current, width, height, scale);
    CGDisplayModeRelease(current);
    if (there)
        return true;
    NSDictionary *options = @{(__bridge id)kCGDisplayShowDuplicateLowResolutionModes : @YES};
    NSArray *modes = CFBridgingRelease(CGDisplayCopyAllDisplayModes(display, (__bridge CFDictionaryRef)options));
    for (id each in modes)
        if (is_mode((__bridge CGDisplayModeRef)each, width, height, scale))
            return CGDisplaySetDisplayMode(display, (__bridge CGDisplayModeRef)each, NULL) == kCGErrorSuccess;
    return false;
}

/* Makes the display, called `name`, of `width` x `height` points at `fps`, each point `scale` (1 or 2) pixels
   each way; NULL if it cannot. Its number goes to *id. */
void *virtual_display_create(const char *name, unsigned width, unsigned height, unsigned scale, unsigned fps, uint32_t *id)
{
    @autoreleasepool {
    CGVirtualDisplayDescriptor *descriptor = [[CGVirtualDisplayDescriptor alloc] init];
    descriptor.queue = dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0);
    descriptor.name = @(name);
    descriptor.maxPixelsWide = descriptor.maxPixelsHigh = LARGEST;
    descriptor.sizeInMillimeters = CGSizeMake(MILLIMETRES_WIDE, MILLIMETRES_HIGH);
    descriptor.vendorID = descriptor.productID = descriptor.serialNum = 1;
    CGVirtualDisplay *display = [[CGVirtualDisplay alloc] initWithDescriptor:descriptor];
    CGVirtualDisplaySettings *settings = [[CGVirtualDisplaySettings alloc] init];
    settings.hiDPI = scale > 1;
    settings.modes = @[ [[CGVirtualDisplayMode alloc] initWithWidth:width height:height refreshRate:fps] ];
    if (!display || ![display applySettings:settings])
        return NULL;
    /* macOS takes a moment to offer the mode, and then starts the display in another. */
    bool ready = false;
    for (int try = 0; try < MODE_TRIES && !ready; try++, usleep(MODE_WAIT))
        ready = in_mode(display.displayID, width, height, scale);
    if (!ready)
        return NULL;
    *id = display.displayID;
    return (__bridge_retained void *)display;
    }
}
