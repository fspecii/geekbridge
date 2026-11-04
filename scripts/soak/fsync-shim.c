/* SPDX-License-Identifier: GPL-3.0-or-later
 * LD_PRELOAD shim for soak diagnosis: logs every fsync/fdatasync of the service as
 * "<tid> <seconds> <path>" to $FSYNC_SHIM_LOG. The service's two async workers are the two threads
 * created right after the main thread (tid main+1, main+2); anything else is the blocking pool.
 *   gcc -shared -fPIC -O2 -o fsync-shim.so scripts/soak/fsync-shim.c -ldl
 *   GEEKBRIDGE_SOAK_PRELOAD=$PWD/fsync-shim.so FSYNC_SHIM_LOG=... scripts/soak-features.sh
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static void note(int fd, double secs) {
    const char *log = getenv("FSYNC_SHIM_LOG");
    if (!log) return;
    char link[64], path[512];
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    path[n > 0 ? n : 0] = 0;
    FILE *f = fopen(log, "a");
    if (!f) return;
    fprintf(f, "%ld %ld %.3f %s\n", (long)getpid(), (long)syscall(SYS_gettid), secs, path);
    fclose(f);
}

#define WRAP(name)                                                                  \
    int name(int fd) {                                                              \
        static int (*real)(int);                                                    \
        if (!real) real = dlsym(RTLD_NEXT, #name);                                  \
        struct timespec a, b;                                                       \
        clock_gettime(CLOCK_MONOTONIC, &a);                                         \
        int r = real(fd);                                                           \
        clock_gettime(CLOCK_MONOTONIC, &b);                                         \
        note(fd, (b.tv_sec - a.tv_sec) + (b.tv_nsec - a.tv_nsec) / 1e9);            \
        return r;                                                                   \
    }
WRAP(fsync)
WRAP(fdatasync)
