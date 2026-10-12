const std = @import("std");
const Io = std.Io;
const Allocator = std.mem.Allocator;
const posix = std.posix;
const c = @cImport({
    // systemd headers pull in glibc inline wrappers that Zig cannot translate
    // with fortification enabled. Only the imported declarations need this.
    @cUndef("_FORTIFY_SOURCE");
    @cInclude("systemd/sd-bus.h");
    @cInclude("systemd/sd-event.h");
});
const name = "org.freedesktop.impl.portal.desktop.kwm";
const path = "/org/freedesktop/portal/desktop";
const iface = "org.freedesktop.impl.portal.Inhibit";
const request_iface = "org.freedesktop.impl.portal.Request";
const frontend = "org.freedesktop.portal.Desktop";
const xml = @embedFile("inhibit.xml");
const request_xml =
    \\<node><interface name="org.freedesktop.impl.portal.Request"><method name="Close"/></interface></node>
;

const Backend = struct {
    allocator: Allocator,
    io: Io,
    user_bus: ?*c.sd_bus = null,
    system_bus: ?*c.sd_bus = null,
    requests: std.StringHashMapUnmanaged(*Request) = .empty,

    fn deinitRequests(self: *Backend) void {
        while (self.requests.count() != 0) {
            var it = self.requests.valueIterator();
            it.next().?.*.destroy();
        }
        self.requests.deinit(self.allocator);
    }
};

const Request = struct {
    backend: *Backend,
    handle: [:0]u8,
    owner: [:0]u8,
    invocation: ?*c.sd_bus_message,
    object_slot: ?*c.sd_bus_slot = null,
    pending_slot: ?*c.sd_bus_slot = null,
    inhibitor: ?Io.File = null,

    fn create(backend: *Backend, handle: []const u8, owner: []const u8, invocation: ?*c.sd_bus_message) !*Request {
        const alloc = backend.allocator;
        const self = try alloc.create(Request);
        errdefer alloc.destroy(self);
        const owned_handle = try alloc.dupeZ(u8, handle);
        errdefer alloc.free(owned_handle);
        const owned_owner = try alloc.dupeZ(u8, owner);
        self.* = .{
            .backend = backend,
            .handle = owned_handle,
            .owner = owned_owner,
            .invocation = c.sd_bus_message_ref(invocation),
        };
        return self;
    }

    fn destroy(self: *Request) void {
        const alloc = self.backend.allocator;
        _ = self.backend.requests.remove(self.handle);
        _ = c.sd_bus_slot_unref(self.pending_slot);
        _ = c.sd_bus_slot_unref(self.object_slot);
        _ = c.sd_bus_message_unref(self.invocation);
        if (self.inhibitor) |file| file.close(self.backend.io);
        alloc.free(self.handle);
        alloc.free(self.owner);
        alloc.destroy(self);
    }
};

fn check(rc: c_int) !void {
    if (rc < 0) {
        std.log.err("sd-bus/system call failed: errno {d}", .{-rc});
        return error.BusFailure;
    }
}
fn equal(a: [*c]const u8, b: []const u8) bool {
    return a != null and std.mem.eql(u8, std.mem.span(a), b);
}
fn fail(m: ?*c.sd_bus_message, error_name: [*:0]const u8, message: [*:0]const u8) c_int {
    return c.sd_bus_reply_method_errorf(m, error_name, "%s", message);
}
fn invalid(m: ?*c.sd_bus_message) c_int {
    return fail(m, "org.freedesktop.DBus.Error.InvalidArgs", "Invalid Inhibit arguments");
}

// Only the portal frontend can supply application identities to this backend.
fn fromFrontend(backend: *Backend, m: ?*c.sd_bus_message) bool {
    var creds: ?*c.sd_bus_creds = null;
    if (c.sd_bus_get_name_creds(backend.user_bus, frontend, c.SD_BUS_CREDS_UNIQUE_NAME, &creds) < 0) return false;
    defer _ = c.sd_bus_creds_unref(creds);
    var owner: [*c]const u8 = null;
    if (c.sd_bus_creds_get_unique_name(creds, &owner) < 0) return false;
    return equal(c.sd_bus_message_get_sender(m), std.mem.span(owner));
}

fn requestCall(m: ?*c.sd_bus_message, data: ?*anyopaque, _: [*c]c.sd_bus_error) callconv(.c) c_int {
    const req: *Request = @ptrCast(@alignCast(data.?));
    if (c.sd_bus_message_is_method_call(m, "org.freedesktop.DBus.Introspectable", "Introspect") > 0)
        return c.sd_bus_reply_method_return(m, "s", request_xml);
    if (c.sd_bus_message_is_method_call(m, request_iface, "Close") <= 0) return 0;
    if (!equal(c.sd_bus_message_get_sender(m), req.owner))
        return fail(m, "org.freedesktop.DBus.Error.AccessDenied", "Request belongs to another caller");
    if (c.sd_bus_message_has_signature(m, "") <= 0) return invalid(m);
    if (req.invocation != null)
        _ = fail(req.invocation, "org.freedesktop.portal.Error.Cancelled", "Inhibition cancelled");
    const rc = c.sd_bus_reply_method_return(m, "");
    req.destroy();
    return rc;
}

fn acquired(m: ?*c.sd_bus_message, data: ?*anyopaque, _: [*c]c.sd_bus_error) callconv(.c) c_int {
    const req: *Request = @ptrCast(@alignCast(data.?));
    req.pending_slot = c.sd_bus_slot_unref(req.pending_slot);
    if (c.sd_bus_message_is_method_error(m, null) > 0) {
        _ = c.sd_bus_reply_method_error(req.invocation, c.sd_bus_message_get_error(m));
        req.destroy();
        return 1;
    }
    var fd: c_int = -1;
    if (c.sd_bus_message_read(m, "h", &fd) <= 0) {
        _ = fail(req.invocation, "org.freedesktop.portal.Error.Failed", "logind returned no inhibitor fd");
        req.destroy();
        return 1;
    }
    // The received fd belongs to the reply message. Keep our own CLOEXEC copy.
    req.inhibitor = retainInhibitor(fd) catch {
        _ = fail(req.invocation, "org.freedesktop.portal.Error.Failed", "Cannot retain inhibitor fd");
        req.destroy();
        return 1;
    };
    const rc = c.sd_bus_reply_method_return(req.invocation, "");
    req.invocation = c.sd_bus_message_unref(req.invocation);
    if (rc < 0) req.destroy();
    return 1;
}

// logind cannot enforce logout/user-switch flags. Reject the entire mask
// rather than silently granting only its supported subset.
fn logindWhat(flags: u32) error{ InvalidFlags, UnsupportedFlags }![:0]const u8 {
    if (flags == 0 or flags & ~@as(u32, 15) != 0) return error.InvalidFlags;
    if (flags & 3 != 0) return error.UnsupportedFlags;
    return if (flags == 12) "sleep:idle" else if (flags == 4) "sleep" else "idle";
}

fn retainInhibitor(fd: posix.fd_t) error{RetainInhibitor}!Io.File {
    // Zig 0.16 has no Io.File duplication API. Keep this OS interop at the
    // ownership boundary, then use Io.File for the lifetime of the inhibitor.
    const flags = std.c.fcntl(fd, posix.F.GETFL);
    if (flags < 0) return error.RetainInhibitor;
    const open_flags: posix.O = @bitCast(@as(u32, @intCast(flags)));
    const owned = std.c.fcntl(fd, posix.F.DUPFD_CLOEXEC, @as(c_int, 3));
    if (owned < 0) return error.RetainInhibitor;
    return .{ .handle = owned, .flags = .{ .nonblocking = open_flags.NONBLOCK } };
}

// Ignore unrelated well-known names owned by the same connection. A frontend
// replacement invalidates requests even if the old connection is still alive.
fn lostOwner(bus_name: []const u8, old: []const u8, new: []const u8) ?[]const u8 {
    if (old.len == 0) return null;
    if (std.mem.eql(u8, bus_name, frontend)) return old;
    if (std.mem.startsWith(u8, bus_name, ":") and new.len == 0) return old;
    return null;
}

fn inhibit(backend: *Backend, m: ?*c.sd_bus_message) !c_int {
    if (c.sd_bus_message_has_signature(m, "ossua{sv}") <= 0) return invalid(m);
    var handle: [*c]const u8 = null;
    var app: [*c]const u8 = null;
    var window: [*c]const u8 = null;
    var flags: u32 = 0;
    if (c.sd_bus_message_read(m, "ossu", &handle, &app, &window, &flags) <= 0) return invalid(m);
    const handle_slice = std.mem.span(handle);
    if (!std.mem.startsWith(u8, handle_slice, "/org/freedesktop/portal/desktop/request/")) return invalid(m);
    const what = logindWhat(flags) catch |err| switch (err) {
        error.InvalidFlags => return invalid(m),
        error.UnsupportedFlags => return fail(m, "org.freedesktop.portal.Error.NotSupported", "Logout and user-switch inhibition are not supported"),
    };
    if (backend.requests.contains(handle_slice))
        return fail(m, "org.freedesktop.DBus.Error.InvalidArgs", "Request handle already exists");
    if (backend.requests.count() >= 1024)
        return fail(m, "org.freedesktop.DBus.Error.LimitsExceeded", "Too many inhibitor requests");

    var reason: [*c]const u8 = "Application requested inhibition";
    if (c.sd_bus_message_enter_container(m, 'a', "{sv}") <= 0) return invalid(m);
    while (true) {
        const rc = c.sd_bus_message_enter_container(m, 'e', "sv");
        if (rc < 0) return invalid(m);
        if (rc == 0) break;
        var key: [*c]const u8 = null;
        if (c.sd_bus_message_read(m, "s", &key) <= 0) return invalid(m);
        if (equal(key, "reason")) {
            if (c.sd_bus_message_enter_container(m, 'v', "s") <= 0) return invalid(m);
            if (c.sd_bus_message_read(m, "s", &reason) <= 0) return invalid(m);
            if (c.sd_bus_message_exit_container(m) < 0) return invalid(m);
        } else if (c.sd_bus_message_skip(m, "v") < 0) return invalid(m);
        if (c.sd_bus_message_exit_container(m) < 0) return invalid(m);
    }
    if (c.sd_bus_message_exit_container(m) < 0) return invalid(m);
    const req = try Request.create(backend, handle_slice, std.mem.span(c.sd_bus_message_get_sender(m)), m);
    errdefer req.destroy();
    try check(c.sd_bus_add_object(backend.user_bus, &req.object_slot, req.handle, requestCall, req));
    try backend.requests.put(backend.allocator, req.handle, req);
    try check(c.sd_bus_call_method_async(backend.system_bus, &req.pending_slot, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager", "Inhibit", acquired, req, "ssss", what.ptr, @as([*c]const u8, if (equal(app, "")) "kwm-portal" else app), reason, @as([*:0]const u8, "block")));
    // Reply only once logind has granted the inhibitor.
    return 1;
}

fn portalCall(m: ?*c.sd_bus_message, data: ?*anyopaque, _: [*c]c.sd_bus_error) callconv(.c) c_int {
    const backend: *Backend = @ptrCast(@alignCast(data.?));
    if (c.sd_bus_message_is_method_call(m, "org.freedesktop.DBus.Introspectable", "Introspect") > 0)
        return c.sd_bus_reply_method_return(m, "s", @as([*:0]const u8, xml));
    if (c.sd_bus_message_is_method_call(m, iface, null) <= 0) return 0;
    if (!fromFrontend(backend, m))
        return fail(m, "org.freedesktop.DBus.Error.AccessDenied", "Only xdg-desktop-portal may call this backend");
    if (c.sd_bus_message_is_method_call(m, iface, "Inhibit") > 0)
        return inhibit(backend, m) catch {
            return fail(m, "org.freedesktop.portal.Error.Failed", "Unable to create inhibitor request");
        };
    if (c.sd_bus_message_is_method_call(m, iface, "CreateMonitor") > 0) {
        if (c.sd_bus_message_has_signature(m, "ooss") <= 0) return invalid(m);
        // A logind inhibitor is not a session lifecycle monitor.
        return c.sd_bus_reply_method_return(m, "u", @as(u32, 2));
    }
    if (c.sd_bus_message_is_method_call(m, iface, "QueryEndResponse") > 0)
        return fail(m, "org.freedesktop.portal.Error.NotSupported", "Session monitoring is not implemented");
    return 0;
}

fn ownerChanged(m: ?*c.sd_bus_message, data: ?*anyopaque, _: [*c]c.sd_bus_error) callconv(.c) c_int {
    const backend: *Backend = @ptrCast(@alignCast(data.?));
    var bus_name: [*c]const u8 = null;
    var old: [*c]const u8 = null;
    var new: [*c]const u8 = null;
    if (c.sd_bus_message_read(m, "sss", &bus_name, &old, &new) <= 0) return 0;
    const lost_owner = lostOwner(std.mem.span(bus_name), std.mem.span(old), std.mem.span(new)) orelse return 0;
    while (true) {
        var it = backend.requests.valueIterator();
        var found: ?*Request = null;
        while (it.next()) |req| {
            if (std.mem.eql(u8, lost_owner, req.*.owner)) {
                found = req.*;
                break;
            }
        }
        const req = found orelse break;
        if (req.invocation != null)
            _ = fail(req.invocation, "org.freedesktop.portal.Error.Cancelled", "Portal frontend disconnected");
        req.destroy();
    }
    return 0;
}

pub fn main(init: std.process.Init) !void {
    const args = try init.minimal.args.toSlice(init.arena.allocator());
    if (args.len == 2 and std.mem.eql(u8, args[1], "--help")) {
        try Io.File.stdout().writeStreamingAll(init.io, "Usage: kwm-portal [--help | --version]\n" ++
            "Forwards portal suspend/idle inhibition to systemd-logind.\n");
        return;
    }
    if (args.len == 2 and std.mem.eql(u8, args[1], "--version")) {
        try Io.File.stdout().writeStreamingAll(init.io, "kwm-portal 0.2.0\n");
        return;
    }
    if (args.len != 1) return error.InvalidArguments;
    var backend: Backend = .{ .allocator = init.gpa, .io = init.io };
    var mask = posix.sigemptyset();
    posix.sigaddset(&mask, .INT);
    posix.sigaddset(&mask, .TERM);
    posix.sigprocmask(posix.SIG.BLOCK, &mask, null);
    var event: ?*c.sd_event = null;
    try check(c.sd_event_default(&event));
    defer _ = c.sd_event_unref(event);
    try check(c.sd_event_add_signal(event, null, @intFromEnum(posix.SIG.INT), null, null));
    try check(c.sd_event_add_signal(event, null, @intFromEnum(posix.SIG.TERM), null, null));
    try check(c.sd_bus_open_user(&backend.user_bus));
    defer _ = c.sd_bus_flush_close_unref(backend.user_bus);
    try check(c.sd_bus_open_system(&backend.system_bus));
    defer _ = c.sd_bus_flush_close_unref(backend.system_bus);
    try check(c.sd_bus_set_exit_on_disconnect(backend.user_bus, 1));
    try check(c.sd_bus_set_exit_on_disconnect(backend.system_bus, 1));
    try check(c.sd_bus_set_method_call_timeout(backend.system_bus, 10 * 1000 * 1000));
    try check(c.sd_bus_attach_event(backend.user_bus, event, 0));
    try check(c.sd_bus_attach_event(backend.system_bus, event, 0));
    defer backend.deinitRequests();
    var object_slot: ?*c.sd_bus_slot = null;
    try check(c.sd_bus_add_object(backend.user_bus, &object_slot, path, portalCall, &backend));
    defer _ = c.sd_bus_slot_unref(object_slot);
    var match_slot: ?*c.sd_bus_slot = null;
    try check(c.sd_bus_match_signal(backend.user_bus, &match_slot, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", ownerChanged, &backend));
    defer _ = c.sd_bus_slot_unref(match_slot);
    try check(c.sd_bus_request_name(backend.user_bus, name, 0));
    std.log.info("Inhibit backend ready", .{});
    try check(c.sd_event_loop(event));
}

test "supported flags map to exact logind operations" {
    try std.testing.expectEqualStrings("sleep", try logindWhat(4));
    try std.testing.expectEqualStrings("idle", try logindWhat(8));
    try std.testing.expectEqualStrings("sleep:idle", try logindWhat(12));
}

test "logout and user switch reject the whole mask" {
    for (1..16) |mask| {
        const flags: u32 = @intCast(mask);
        if (flags & 3 != 0)
            try std.testing.expectError(error.UnsupportedFlags, logindWhat(flags));
    }
}

test "zero and every unknown flag bit are invalid" {
    try std.testing.expectError(error.InvalidFlags, logindWhat(0));
    for (4..32) |bit| {
        const unknown = @as(u32, 1) << @intCast(bit);
        for (0..16) |known| {
            try std.testing.expectError(error.InvalidFlags, logindWhat(unknown | @as(u32, @intCast(known))));
        }
    }
    try std.testing.expectError(error.InvalidFlags, logindWhat(std.math.maxInt(u32)));
}

test "only frontend name loss and unique-name disconnect release requests" {
    try std.testing.expectEqualStrings(":1.20", lostOwner(frontend, ":1.20", "").?);
    try std.testing.expectEqualStrings(":1.20", lostOwner(frontend, ":1.20", ":1.21").?);
    try std.testing.expectEqualStrings(":1.20", lostOwner(":1.20", ":1.20", "").?);
    try std.testing.expectEqual(null, lostOwner(frontend, "", ":1.20"));
    try std.testing.expectEqual(null, lostOwner(":1.20", "", ":1.20"));
    try std.testing.expectEqual(null, lostOwner("org.example.Unrelated", ":1.20", ""));
    try std.testing.expectEqual(null, lostOwner("org.example.Unrelated", ":1.20", ":1.21"));
}

test "retained inhibitor survives original fd closure and is close-on-exec" {
    const io = std.testing.io;
    var pipe: [2]posix.fd_t = undefined;
    try std.testing.expectEqual(@as(c_int, 0), std.c.pipe2(&pipe, .{ .NONBLOCK = true, .CLOEXEC = true }));
    const reader: Io.File = .{ .handle = pipe[0], .flags = .{ .nonblocking = true } };
    defer reader.close(io);
    var original: ?Io.File = .{ .handle = pipe[1], .flags = .{ .nonblocking = true } };
    defer if (original) |file| file.close(io);
    var retained: ?Io.File = try retainInhibitor(original.?.handle);
    defer if (retained) |file| file.close(io);
    try std.testing.expect(retained.?.handle != original.?.handle);
    try std.testing.expect(retained.?.flags.nonblocking);
    try std.testing.expect(std.c.fcntl(retained.?.handle, posix.F.GETFD) & posix.FD_CLOEXEC != 0);
    original.?.close(io);
    original = null;
    var bytes: [1]u8 = undefined;
    // Use nonblocking read here to distinguish a retained writer from EOF.
    try std.testing.expectEqual(@as(isize, -1), std.c.read(reader.handle, &bytes, bytes.len));
    retained.?.close(io);
    retained = null;
    try std.testing.expectEqual(@as(isize, 0), std.c.read(reader.handle, &bytes, bytes.len));
    try std.testing.expectError(error.RetainInhibitor, retainInhibitor(-1));
}

test "destroying one request releases only its fd and registry entry" {
    var backend: Backend = .{ .allocator = std.testing.allocator, .io = std.testing.io };
    defer backend.deinitRequests();
    var pipe: [2]posix.fd_t = undefined;
    try std.testing.expectEqual(@as(c_int, 0), std.c.pipe2(&pipe, .{ .CLOEXEC = true }));
    const reader: Io.File = .{ .handle = pipe[0], .flags = .{ .nonblocking = false } };
    const writer: Io.File = .{ .handle = pipe[1], .flags = .{ .nonblocking = false } };
    defer reader.close(backend.io);
    defer writer.close(backend.io);
    for ([_][]const u8{ "/request/one", "/request/two" }) |handle| {
        const req = try Request.create(&backend, handle, ":1.20", null);
        errdefer req.destroy();
        req.inhibitor = try retainInhibitor(writer.handle);
        try backend.requests.put(backend.allocator, req.handle, req);
    }
    const first = backend.requests.get("/request/one").?;
    const first_fd = first.inhibitor.?.handle;
    const second = backend.requests.get("/request/two").?;
    first.destroy();
    try std.testing.expectEqual(@as(c_int, -1), std.c.fcntl(first_fd, posix.F.GETFD));
    try std.testing.expect(!backend.requests.contains("/request/one"));
    try std.testing.expectEqual(@as(u32, 1), backend.requests.count());
    try std.testing.expectEqual(second, backend.requests.get("/request/two").?);
    try std.testing.expect(std.c.fcntl(second.inhibitor.?.handle, posix.F.GETFD) >= 0);
}

test "pending request without fd can be destroyed and its handle reused" {
    var backend: Backend = .{ .allocator = std.testing.allocator, .io = std.testing.io };
    defer backend.deinitRequests();
    for (0..2) |_| {
        const req = try Request.create(&backend, "/request/pending", ":1.20", null);
        defer req.destroy();
        try backend.requests.put(backend.allocator, req.handle, req);
        try std.testing.expectEqual(null, req.inhibitor);
        try std.testing.expectEqual(@as(u32, 1), backend.requests.count());
    }
    try std.testing.expectEqual(@as(u32, 0), backend.requests.count());
}

test "request creation and registration clean up on allocation failure" {
    const scenario = struct {
        fn run(allocator: Allocator) !void {
            var backend: Backend = .{ .allocator = allocator, .io = std.testing.io };
            defer backend.deinitRequests();
            const req = try Request.create(&backend, "/request/oom", ":1.20", null);
            errdefer req.destroy();
            try backend.requests.put(allocator, req.handle, req);
        }
    };
    try std.testing.checkAllAllocationFailures(std.testing.allocator, scenario.run, .{});
}
