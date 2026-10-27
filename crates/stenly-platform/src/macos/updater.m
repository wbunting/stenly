// Optional Sparkle 2 adapter. Built only with an explicitly supplied framework.
// Sparkle owns signature verification and transactional installation. No custom
// archive extraction, privileged shell installer, or unsigned fallback lives here.
#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>

typedef bool (*AcquireInstall)(void);
typedef void (*ReleaseInstall)(void);
typedef bool (*IsBusy)(void);
static AcquireInstall acquireInstall;
static ReleaseInstall releaseInstall;
static IsBusy isBusy;
static BOOL enabled;
static BOOL leased;
static BOOL testing;
static BOOL cancelRequested;
static NSString *availableVersion;
static NSString *availableURL;
static NSInteger lastError;
static SPUUpdater *updater;
static NSString *validatedFeed;
static void (^cancelDownload)(void);
static void (^cancelPrepared)(void);

static BOOL SafeURL(NSURL *url) {
    if (!url || url.user || url.password || url.fragment || url.query) return NO;
    if ([url.scheme isEqualToString:@"https"]) return YES;
    return testing && [url.scheme isEqualToString:@"http"] &&
        ([@[@"127.0.0.1", @"localhost", @"::1"] containsObject:url.host]);
}
static BOOL Allowed(NSError **error) {
    if (enabled && !isBusy()) return YES;
    if (error) *error = [NSError errorWithDomain:@"ai.stenly.updates" code:1 userInfo:@{NSLocalizedDescriptionKey:@"Updates are disabled or a recording/dictation is active. Try again when idle."}];
    return NO;
}
static void Release(void) {
    if (leased) { leased = NO; releaseInstall(); }
}
static BOOL Acquire(void) {
    if (!enabled) return NO;
    if (leased) return YES;
    leased = acquireInstall();
    return leased;
}

@interface StenlyUpdateDelegate : NSObject <SPUUpdaterDelegate>
@end
@implementation StenlyUpdateDelegate
- (NSString *)feedURLStringForUpdater:(SPUUpdater *)sender { return validatedFeed; }
- (BOOL)updaterShouldPromptForPermissionToCheckForUpdates:(SPUUpdater *)sender { return NO; }
- (NSArray<NSString *> *)allowedSystemProfileKeysForUpdater:(SPUUpdater *)sender { return @[]; }
- (BOOL)updater:(SPUUpdater *)sender mayPerformUpdateCheck:(SPUUpdateCheck)check error:(NSError **)error { return Allowed(error); }
- (BOOL)updater:(SPUUpdater *)sender shouldProceedWithUpdate:(SUAppcastItem *)item updateCheck:(SPUUpdateCheck)check error:(NSError **)error {
    NSDictionary *enclosure = item.propertiesDictionary[@"enclosure"];
    NSString *signature = enclosure[@"sparkle:edSignature"];
    NSData *decoded = signature ? [[NSData alloc] initWithBase64EncodedString:signature options:0] : nil;
    BOOL valid = SafeURL(item.fileURL) && decoded.length == 64 && [item.installationType isEqualToString:@"application"];
    if (!valid && error) *error = [NSError errorWithDomain:@"ai.stenly.updates" code:2 userInfo:@{NSLocalizedDescriptionKey:@"The update must be a signed application archive from a secure release feed."}];
    return valid && Allowed(error);
}
- (BOOL)updater:(SPUUpdater *)sender shouldDownloadReleaseNotesForUpdate:(SUAppcastItem *)item { return NO; }
- (void)updater:(SPUUpdater *)sender didFindValidUpdate:(SUAppcastItem *)item {
    if (enabled && SafeURL(item.fileURL)) { availableVersion = item.displayVersionString; availableURL = item.fileURL.absoluteString; }
}
- (BOOL)updaterShouldRelaunchApplication:(SPUUpdater *)sender {
#ifdef STENLY_UPDATER_TEST_INSTALLER
    return testing && enabled && leased;
#else
    return NO;
#endif
}
- (void)updater:(SPUUpdater *)sender didAbortWithError:(NSError *)error { lastError = error.code; Release(); }
- (void)updater:(SPUUpdater *)sender didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error { lastError = error ? error.code : 0; if (error || cancelRequested) Release(); cancelRequested = NO; }
@end

// Installation remains a local-fixture experiment until extraction-stage
// cancellation is proven. Normal Cargo builds do not compile this driver.
#ifdef STENLY_UPDATER_TEST_INSTALLER
// Intercept both install choices at the last synchronous boundary. Once a
// lease is granted the Rust capture entry points reject new recording starts.
// The lease begins before downloading: Sparkle can prepare an installer
// listening for normal host termination before its final relaunch button.
@interface StenlyUpdateDriver : SPUStandardUserDriver
@end
@implementation StenlyUpdateDriver
- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    [super showUpdateFoundWithAppcastItem:item state:state reply:^(SPUUserUpdateChoice choice) {
        // Sparkle prepares an on-quit installer after extraction. Own the
        // lease from the first download/install choice, not only relaunch.
        if (choice == SPUUserUpdateChoiceInstall && !Acquire()) choice = SPUUserUpdateChoiceSkip;
        if (choice != SPUUserUpdateChoiceInstall) {
            if (state.stage == SPUUserUpdateStageInstalling) choice = SPUUserUpdateChoiceSkip;
            cancelRequested = YES;
        }
        reply(choice);
    }];
}
- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation {
    cancelDownload = ^{ cancelRequested = YES; cancellation(); };
    [super showDownloadInitiatedWithCancellation:cancelDownload];
}
- (void)showDownloadDidStartExtractingUpdate {
    cancelDownload = nil;
    [super showDownloadDidStartExtractingUpdate];
}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    cancelPrepared = ^{ cancelRequested = YES; reply(SPUUserUpdateChoiceSkip); };
    if (!enabled) { cancelPrepared(); cancelPrepared = nil; return; }
    [super showReadyToInstallAndRelaunch:^(SPUUserUpdateChoice choice) {
        cancelPrepared = nil;
        if (choice == SPUUserUpdateChoiceInstall && !Acquire()) choice = SPUUserUpdateChoiceSkip;
        if (choice != SPUUserUpdateChoiceInstall) { choice = SPUUserUpdateChoiceSkip; cancelRequested = YES; }
        reply(choice);
    }];
}
- (void)dismissUpdateInstallation { cancelDownload = nil; cancelPrepared = nil; [super dismissUpdateInstallation]; }
@end
#endif
static StenlyUpdateDelegate *delegate;
static SPUStandardUserDriver *driver;

// Return 0 when absent/unconfigured, 1 when initialized, -1 on start failure.
int stenly_updater_start(bool optIn, AcquireInstall acquire, ReleaseInstall release, IsBusy busy, bool allowTestFeed) {
    if (![NSThread isMainThread]) return -1;
    if (updater) return 1;
    NSBundle *bundle = NSBundle.mainBundle;
#if defined(STENLY_UPDATER_TEST_INSTALLER) || defined(STENLY_UPDATER_TEST_FEED)
    testing = allowTestFeed && [bundle.bundleIdentifier hasPrefix:@"ai.stenly.updater-test."];
#else
    testing = NO;
#endif
    NSURL *feed = [NSURL URLWithString:bundle.infoDictionary[@"SUFeedURL"] ?: @""];
    NSData *key = [[NSData alloc] initWithBase64EncodedString:bundle.infoDictionary[@"SUPublicEDKey"] ?: @"" options:0];
    // Do not show Sparkle's misconfiguration alert or touch the network until
    // a real feed and public verification key have been deliberately bundled.
    if (!SafeURL(feed) || key.length != 32 || !acquire || !release || !busy) return 0;
    // Fail closed if packaging forgot privacy and manual-install policy keys.
    // NSURLSession ATS also covers redirect targets. Never allow release
    // packaging to turn ATS off; the fixture's loopback exception is explicit.
    if (!testing && bundle.infoDictionary[@"NSAppTransportSecurity"] != nil) return 0;
    if ([bundle.infoDictionary[@"SUSendProfileInfo"] boolValue] ||
        ![bundle.infoDictionary[@"SUAllowsAutomaticUpdates"] isEqual:@NO] ||
        ![bundle.infoDictionary[@"SUVerifyUpdateBeforeExtraction"] isEqual:@YES]) return 0;
    validatedFeed = feed.absoluteString;
    acquireInstall = acquire; releaseInstall = release; isBusy = busy;
    enabled = optIn; lastError = 0; cancelRequested = NO;
    delegate = [StenlyUpdateDelegate new];
#ifdef STENLY_UPDATER_TEST_INSTALLER
    driver = [[StenlyUpdateDriver alloc] initWithHostBundle:bundle delegate:nil];
#else
    driver = [[SPUStandardUserDriver alloc] initWithHostBundle:bundle delegate:nil];
#endif
    updater = [[SPUUpdater alloc] initWithHostBundle:bundle applicationBundle:bundle userDriver:driver delegate:delegate];
    // User-controlled checks only; Sparkle's system-profile setting is never
    // enabled by this adapter and allowedSystemProfileKeys always returns [].
    // App-owned probing checks only. Sparkle's scheduled UI checks could
    // otherwise offer installation without the proven cancellation contract.
    updater.automaticallyChecksForUpdates = NO;
    updater.automaticallyDownloadsUpdates = NO;
    NSError *error = nil;
    if (![updater startUpdater:&error]) { lastError = error.code; updater = nil; return -1; }
    return 1;
}
void stenly_updater_enabled(bool value) {
    // No queued restart or updater replacement: latest desired state wins.
    enabled = value;
    updater.automaticallyChecksForUpdates = NO;
    updater.automaticallyDownloadsUpdates = NO;
    if (!value) {
        availableVersion = nil; availableURL = nil;
#ifdef STENLY_UPDATER_TEST_INSTALLER
        cancelRequested = YES;
        if (cancelDownload) { void (^cancel)(void) = cancelDownload; cancelDownload = nil; cancel(); }
        if (cancelPrepared) { void (^cancel)(void) = cancelPrepared; cancelPrepared = nil; cancel(); }
        // Extraction-gap cancellation is intentionally NOT claimed working.
        // This installer exists only in explicit local fixture builds.
#endif
    }
}
void stenly_updater_check(void) {
    if (!updater || !enabled || !isBusy || isBusy()) return;
#ifdef STENLY_UPDATER_TEST_INSTALLER
    if (testing) { [updater checkForUpdates]; return; }
#endif
    [updater checkForUpdateInformation];
}
long stenly_updater_error(void) { return lastError; }
const char *stenly_updater_available_version(void) { return availableVersion.UTF8String; }
const char *stenly_updater_available_url(void) { return availableURL.UTF8String; }
