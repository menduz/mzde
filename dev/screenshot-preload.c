// Fixed values for the widgets of mzde and the sandbox of each window in the
// screenshots. The screenshot script loads this library into kwm with
// LD_PRELOAD, after the library of kwm (nix/screenshot-preload.c in kwm). The
// widgets of the bar are child processes of kwm, and they also get it.
//
//   - /proc/diskstats: no disks. The busy disks widget shows nothing, also
//     when the disks of the build machine are busy.
//   - /etc/sandboxes/config.json: the file in MZDE_SANDBOXES_CONFIG, if that
//     variable is set.
//   - /proc/<pid>/cgroup, if MZDE_SANDBOXES_CONFIG is set: a process with
//     SANDBOX_NAME=<name> in its environment is in the slice of the sandbox
//     <name>. kwm reads the slice, as for a real sandbox. Refer to
//     src/kwm/sandbox.zig in kwm.
//
// The other files and paths go to the C library.

#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static int (*next_open)(const char *, int, ...);

static int real_open(const char *path, int flags) {
    if (!next_open) next_open = dlsym(RTLD_NEXT, "open");
    return next_open(path, flags);
}

// A file in memory that holds `text`, open for reading at its start.
static int memory_file(const char *text) {
    int fd = memfd_create("mzde-screenshot", MFD_CLOEXEC);
    if (fd < 0) return -1;
    size_t len = strlen(text);
    if (write(fd, text, len) != (ssize_t)len) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    return fd;
}

// Write the value of SANDBOX_NAME of the process `pid` into `name`. Return 0
// if the process has no such variable.
static int sandbox_name(int pid, char *name, size_t size) {
    char path[64];
    snprintf(path, sizeof path, "/proc/%d/environ", pid);
    int fd = real_open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;
    static char env[256 * 1024];
    size_t len = 0;
    ssize_t n;
    while (len < sizeof env - 1 && (n = read(fd, env + len, sizeof env - 1 - len)) > 0)
        len += n;
    close(fd);
    env[len] = '\0';

    const char key[] = "SANDBOX_NAME=";
    for (size_t i = 0; i < len; i += strlen(env + i) + 1) {
        if (strncmp(env + i, key, sizeof key - 1) == 0) {
            snprintf(name, size, "%s", env + i + sizeof key - 1);
            return name[0] != '\0' && strcmp(name, "host") != 0;
        }
    }
    return 0;
}

// The file descriptor for a path with a fixed value, or -2 for the other
// paths.
static int fixed_file(const char *path) {
    if (strcmp(path, "/proc/diskstats") == 0) return memory_file("");

    const char *config = getenv("MZDE_SANDBOXES_CONFIG");
    if (!config) return -2;
    if (strcmp(path, "/etc/sandboxes/config.json") == 0)
        return real_open(config, O_RDONLY | O_CLOEXEC);

    int pid, end = 0;
    if (sscanf(path, "/proc/%d/cgroup%n", &pid, &end) == 1 && path[end] == '\0') {
        char name[64], text[256];
        if (sandbox_name(pid, name, sizeof name))
            snprintf(text, sizeof text,
                     "0::/user.slice/user-1000.slice/user@1000.service/"
                     "sandbox.slice/sandbox-%s.slice/run.scope\n", name);
        else
            snprintf(text, sizeof text,
                     "0::/user.slice/user-1000.slice/user@1000.service/"
                     "app.slice/app.scope\n");
        return memory_file(text);
    }
    return -2;
}

#define OPEN_WRAPPER(name)                                                    \
    int name(const char *path, int flags, ...) {                              \
        mode_t mode = 0;                                                      \
        if (flags & (O_CREAT | O_TMPFILE)) {                                  \
            va_list args;                                                     \
            va_start(args, flags);                                            \
            mode = va_arg(args, mode_t);                                      \
            va_end(args);                                                     \
        }                                                                     \
        int fd = fixed_file(path);                                            \
        if (fd != -2) return fd;                                              \
        int (*next)(const char *, int, ...) = dlsym(RTLD_NEXT, #name);        \
        return next(path, flags, mode);                                       \
    }

#define OPENAT_WRAPPER(name)                                                  \
    int name(int dirfd, const char *path, int flags, ...) {                   \
        mode_t mode = 0;                                                      \
        if (flags & (O_CREAT | O_TMPFILE)) {                                  \
            va_list args;                                                     \
            va_start(args, flags);                                            \
            mode = va_arg(args, mode_t);                                      \
            va_end(args);                                                     \
        }                                                                     \
        int fd = fixed_file(path);                                            \
        if (fd != -2) return fd;                                              \
        int (*next)(int, const char *, int, ...) = dlsym(RTLD_NEXT, #name);   \
        return next(dirfd, path, flags, mode);                                \
    }

OPEN_WRAPPER(open)
OPEN_WRAPPER(open64)
OPENAT_WRAPPER(openat)
OPENAT_WRAPPER(openat64)
