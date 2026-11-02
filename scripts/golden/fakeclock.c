/* SPDX-License-Identifier: GPL-3.0-or-later
 * LD_PRELOAD shim that freezes the wall clock (CLOCK_REALTIME, time, gettimeofday) at $GOLDEN_NOW (unix
 * seconds) so "5 min ago", today's calendar cell and the like render identically on every run. Monotonic
 * clocks are untouched, so timeouts still work. Used only by scripts/golden-desktop.sh.
 *   gcc -shared -fPIC -O2 -o fakeclock.so fakeclock.c -ldl */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>

static long now_s(void) { const char *e = getenv("GOLDEN_NOW"); return e ? atol(e) : 0; }

int clock_gettime(clockid_t id, struct timespec *ts) {
    static int (*real)(clockid_t, struct timespec *);
    if (!real) real = dlsym(RTLD_NEXT, "clock_gettime");
    int r = real(id, ts);
    if (r == 0 && (id == CLOCK_REALTIME || id == CLOCK_REALTIME_COARSE) && now_s()) { ts->tv_sec = now_s(); ts->tv_nsec = 0; }
    return r;
}
int gettimeofday(struct timeval *tv, void *tz) {
    static int (*real)(struct timeval *, void *);
    if (!real) real = dlsym(RTLD_NEXT, "gettimeofday");
    int r = real(tv, tz);
    if (r == 0 && tv && now_s()) { tv->tv_sec = now_s(); tv->tv_usec = 0; }
    return r;
}
time_t time(time_t *t) {
    long n = now_s();
    if (!n) { static time_t (*real)(time_t *); if (!real) real = dlsym(RTLD_NEXT, "time"); return real(t); }
    if (t) *t = n;
    return n;
}
