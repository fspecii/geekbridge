/* SPDX-License-Identifier: GPL-3.0-or-later
 * LD_PRELOAD shim for scripts/chaos-crash.sh (test sandboxes only):
 *
 *  - Wall clock: GB_SHIM_TIME_FILE names an 8-byte file holding a signed number of seconds that is added to
 *    CLOCK_REALTIME / time() / gettimeofday(). The file is mapped, so the script can step the clock of a running
 *    service forwards and backwards. CLOCK_MONOTONIC is untouched, as with a real clock step.
 *  - Disk full: while the file named by GB_SHIM_ENOSPC_FLAG exists, write() to a descriptor whose path is under
 *    GB_SHIM_ENOSPC_DIR fails with ENOSPC. Removing the flag "frees" the space. Creating files and renaming
 *    still work, like on a full disk that still has inodes.
 *
 *   gcc -shared -fPIC -O2 -o shim.so scripts/chaos/shim.c -ldl
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static volatile long long *off;
static const char *flag, *dir;

__attribute__((constructor)) static void init(void) {
    const char *t = getenv("GB_SHIM_TIME_FILE");
    if (t) {
        int fd = open(t, O_RDONLY);
        if (fd >= 0) {
            void *m = mmap(0, 8, PROT_READ, MAP_SHARED, fd, 0);
            if (m != MAP_FAILED) off = m;
        }
    }
    flag = getenv("GB_SHIM_ENOSPC_FLAG");
    dir = getenv("GB_SHIM_ENOSPC_DIR");
}

int clock_gettime(clockid_t c, struct timespec *ts) {
    static int (*real)(clockid_t, struct timespec *);
    if (!real) real = dlsym(RTLD_NEXT, "clock_gettime");
    int r = real(c, ts);
    if (r == 0 && off && (c == CLOCK_REALTIME || c == CLOCK_REALTIME_COARSE)) ts->tv_sec += *off;
    return r;
}

time_t time(time_t *out) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    if (out) *out = ts.tv_sec;
    return ts.tv_sec;
}

int gettimeofday(struct timeval *tv, void *tz) {
    static int (*real)(struct timeval *, void *);
    if (!real) real = dlsym(RTLD_NEXT, "gettimeofday");
    int r = real(tv, tz);
    if (r == 0 && off) tv->tv_sec += *off;
    return r;
}

ssize_t write(int fd, const void *buf, size_t n) {
    static ssize_t (*real)(int, const void *, size_t);
    if (!real) real = dlsym(RTLD_NEXT, "write");
    if (flag && dir && n > 0 && fd > 2 && access(flag, F_OK) == 0) {
        char link[64], path[4096];
        snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
        ssize_t k = readlink(link, path, sizeof path - 1);
        if (k > 0) {
            path[k] = 0;
            if (strncmp(path, dir, strlen(dir)) == 0) {
                errno = ENOSPC;
                return -1;
            }
        }
    }
    return real(fd, buf, n);
}
