// The Portduino idle (radio/portduino/idle.cpp) in a program of its own, on
// the wall clock: each check prints "ok <name>" or "fail <name> <why>".
// Built with the link wraps, it checks them too; built without (argument
// "nowrap"), only what needs none.
#include <arpa/inet.h>
#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <netinet/in.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

void rnode_wake();
void rnode_idle(uint32_t max_ms);
void rnode_idle_radio(uint32_t max_ms);
int rnode_idle_fd_ready();

static int64_t now_ms()
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int failures;

static void check(const char* name, bool ok, const char* why)
{
    printf(ok ? "ok %s\n" : "fail %s %s\n", name, why);
    if (!ok)
        failures++;
}

// How long an idle took, in ms, and whether it reported a descriptor.
static int64_t timed(void (*fn)(uint32_t), uint32_t ms, int* ready)
{
    int64_t t0 = now_ms();
    fn(ms);
    int64_t took = now_ms() - t0;
    *ready = rnode_idle_fd_ready();
    return took;
}

static int connect_to(const struct sockaddr_in& addr)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (connect(fd, (const struct sockaddr*)&addr, sizeof addr) != 0)
        return -1;
    return fd;
}

int main(int argc, char** argv)
{
    bool wrapped = !(argc > 1 && strcmp(argv[1], "nowrap") == 0);
    int ready;
    int64_t took;

    took = timed(rnode_idle, 0, &ready);
    check("zero", took < 20, "an idle of 0 ms waited");

    took = timed(rnode_idle, 150, &ready);
    check("timeout", took >= 140 && !ready, "an idle with nothing to end it did not last");

    rnode_wake();
    took = timed(rnode_idle, 1000, &ready);
    check("wake", took < 100 && !ready, "a wake did not end the next idle, or reported a descriptor");

    int lfd = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(lfd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in any = {};
    any.sin_family = AF_INET;
    any.sin_addr.s_addr = htonl(INADDR_ANY);
    any.sin_port = 0;
    if (bind(lfd, (struct sockaddr*)&any, sizeof any) != 0 || listen(lfd, 5) != 0) {
        check("listen", false, strerror(errno));
        return 1;
    }
    struct sockaddr_in bound = {};
    socklen_t blen = sizeof bound;
    getsockname(lfd, (struct sockaddr*)&bound, &blen);
    char text[32];
    inet_ntop(AF_INET, &bound.sin_addr, text, sizeof text);
    if (!wrapped) {
        check("bind", strcmp(text, "0.0.0.0") == 0, text);
        close(lfd);
        return failures ? 1 : 0;
    }
    check("bind", strcmp(text, "127.0.0.2") == 0, text);

    // A connection waiting on the listener ends the idle.
    int cfd = connect_to(bound);
    took = timed(rnode_idle, 1000, &ready);
    check("listener", took < 100 && ready, "a pending connection did not end the idle");
    int afd = accept(lfd, nullptr, nullptr);

    // Bytes on the accepted descriptor end the idle and are reported.
    ssize_t w = write(cfd, "x", 1);
    (void)w;
    took = timed(rnode_idle, 1000, &ready);
    check("readable", took < 100 && ready, "bytes on an accepted socket did not end the idle");

    // ... but not the radio's idle.
    took = timed(rnode_idle_radio, 150, &ready);
    check("radio", took >= 140 && !ready, "the radio's idle ended on a descriptor");
    char c;
    ssize_t r = read(afd, &c, 1);
    (void)r;

    // A wake ends the idle without reporting a descriptor.
    rnode_wake();
    took = timed(rnode_idle, 1000, &ready);
    check("wake-watched", took < 100 && !ready, "a wake reported a descriptor");

    // The peer gone: reported once, then no longer ending idles.
    close(cfd);
    took = timed(rnode_idle, 1000, &ready);
    check("hangup", took < 100 && ready, "the peer's close did not end the idle");
    took = timed(rnode_idle, 150, &ready);
    check("hangup-once", took >= 140 && !ready, "a gone peer still ends idles");
    close(afd);

    // A closed descriptor is no longer watched, its bytes unread or not.
    cfd = connect_to(bound);
    took = timed(rnode_idle, 1000, &ready);
    afd = accept(lfd, nullptr, nullptr);
    w = write(cfd, "y", 1);
    usleep(20000);
    close(afd);
    took = timed(rnode_idle, 150, &ready);
    check("close", took >= 140 && !ready, "a closed descriptor ended the idle");
    close(cfd);
    close(lfd);
    took = timed(rnode_idle, 150, &ready);
    check("closed-listener", took >= 140 && !ready, "a closed listener ended the idle");
    return failures ? 1 : 0;
}
