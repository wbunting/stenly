#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
extern int stenly_updater_start(bool, bool (*)(void), void (*)(void), bool (*)(void), bool);
extern void stenly_updater_check(void);
extern void stenly_updater_enabled(bool);
extern const char *stenly_updater_available_version(void);
static bool leased;
static bool busy(void);
static bool acquire(void) { if (busy()) return false; leased=true; return true; }
static void release(void) { leased=false; }
static bool busy(void) { NSString *path = NSProcessInfo.processInfo.environment[@"STENLY_UPDATE_TEST_BUSY_FILE"];
 return path && [[NSFileManager defaultManager] fileExistsAtPath:path]; }
@interface AppDelegate : NSObject <NSApplicationDelegate>
@property NSWindow *window;
@end
@implementation AppDelegate
- (void)applicationDidFinishLaunching:(NSNotification *)note {
 self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(400,400,500,220) styleMask:NSWindowStyleMaskTitled|NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
 self.window.title = [NSString stringWithFormat:@"Stenly updater synthetic build %@",NSBundle.mainBundle.infoDictionary[@"CFBundleVersion"]];
 NSTextField *label = [NSTextField labelWithString:@"Synthetic updater fixture — no recordings, no user data."];
 label.frame=NSMakeRect(20,130,460,35); [self.window.contentView addSubview:label];
 NSButton *button=[NSButton buttonWithTitle:@"Check signed test update" target:self action:@selector(check:)];button.frame=NSMakeRect(30,60,230,35);[self.window.contentView addSubview:button];
 NSButton *disable=[NSButton buttonWithTitle:@"Disable updates" target:self action:@selector(disable:)]; disable.frame=NSMakeRect(280,60,170,35);[self.window.contentView addSubview:disable];
 [NSTimer scheduledTimerWithTimeInterval:0.2 target:self selector:@selector(testControl:) userInfo:nil repeats:YES];
 [self.window makeKeyAndOrderFront:nil];[NSApp activateIgnoringOtherApps:YES];
 int result = stenly_updater_start(true,acquire,release,busy,true);
 NSLog(@"fixture updater start %d",result);
}
- (void)testControl:(id)sender {
 const char *version = stenly_updater_available_version();
 if (version) self.window.title = [NSString stringWithFormat:@"Synthetic build %@ — available %s (probe only)", NSBundle.mainBundle.infoDictionary[@"CFBundleVersion"], version];
 NSString *path = NSProcessInfo.processInfo.environment[@"STENLY_UPDATE_TEST_DISABLE_FILE"];
 if (path && [[NSFileManager defaultManager] fileExistsAtPath:path]) { stenly_updater_enabled(false); [[NSFileManager defaultManager] removeItemAtPath:path error:nil]; }
 NSString *enable = NSProcessInfo.processInfo.environment[@"STENLY_UPDATE_TEST_ENABLE_FILE"];
 if (enable && [[NSFileManager defaultManager] fileExistsAtPath:enable]) { stenly_updater_enabled(true); [[NSFileManager defaultManager] removeItemAtPath:enable error:nil]; }
}
- (void)check:(id)sender { stenly_updater_check(); }
- (void)disable:(id)sender { stenly_updater_enabled(false); }

@end
int main() { @autoreleasepool { NSApplication *app=NSApplication.sharedApplication;[app setActivationPolicy:NSApplicationActivationPolicyRegular];AppDelegate *delegate=[AppDelegate new];app.delegate=delegate;[app run]; } }
