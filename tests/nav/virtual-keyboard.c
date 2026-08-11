#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <linux/input-event-codes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>

#include "virtual-keyboard-protocol.h"

static struct wl_seat *seat;
static struct zwp_virtual_keyboard_manager_v1 *manager;

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data;
    (void)version;
    if (strcmp(interface, wl_seat_interface.name) == 0 && !seat)
        seat = wl_registry_bind(registry, name, &wl_seat_interface, 1);
    if (strcmp(interface, zwp_virtual_keyboard_manager_v1_interface.name) == 0)
        manager = wl_registry_bind(
            registry, name, &zwp_virtual_keyboard_manager_v1_interface, 1);
}

static void global_remove(void *data, struct wl_registry *registry,
                          uint32_t name) {
    (void)data;
    (void)registry;
    (void)name;
}

static const struct wl_registry_listener registry_listener = {
    .global = global,
    .global_remove = global_remove,
};

static uint32_t now_ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint32_t)(now.tv_sec * 1000ULL + now.tv_nsec / 1000000ULL);
}

static void sleep_ms(unsigned ms) {
    struct timespec left = {
        .tv_sec = ms / 1000,
        .tv_nsec = (long)(ms % 1000) * 1000000L,
    };
    while (nanosleep(&left, &left) < 0 && errno == EINTR) {
    }
}

static int keycode(const char *name) {
    if (strcmp(name, "left") == 0)
        return KEY_LEFT;
    if (strcmp(name, "right") == 0)
        return KEY_RIGHT;
    if (strcmp(name, "up") == 0)
        return KEY_UP;
    if (strcmp(name, "down") == 0)
        return KEY_DOWN;
    if (strcmp(name, "escape") == 0)
        return KEY_ESC;
    fprintf(stderr, "unknown key: %s\n", name);
    return -1;
}

static int send_keymap(struct zwp_virtual_keyboard_v1 *keyboard) {
    struct xkb_context *context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    if (!context)
        return -1;
    struct xkb_rule_names names = {
        .rules = "evdev",
        .model = "pc105",
        .layout = "us",
    };
    struct xkb_keymap *keymap = xkb_keymap_new_from_names(
        context, &names, XKB_KEYMAP_COMPILE_NO_FLAGS);
    if (!keymap) {
        xkb_context_unref(context);
        return -1;
    }
    char *text = xkb_keymap_get_as_string(keymap, XKB_KEYMAP_FORMAT_TEXT_V1);
    size_t size = text ? strlen(text) + 1 : 0;
    int fd = memfd_create("wl-wysiwyc-keymap", MFD_CLOEXEC);
    if (!text || fd < 0 || write(fd, text, size) != (ssize_t)size) {
        free(text);
        if (fd >= 0)
            close(fd);
        xkb_keymap_unref(keymap);
        xkb_context_unref(context);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    zwp_virtual_keyboard_v1_keymap(
        keyboard, WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1, fd, (uint32_t)size);
    close(fd);
    free(text);
    xkb_keymap_unref(keymap);
    xkb_context_unref(context);
    return 0;
}

int main(int argc, char **argv) {
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) {
        fprintf(stderr, "cannot connect to Wayland\n");
        return 1;
    }
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    wl_display_roundtrip(display);
    if (!seat || !manager) {
        fprintf(stderr, "virtual keyboard protocol or seat is unavailable\n");
        return 1;
    }
    struct zwp_virtual_keyboard_v1 *keyboard =
        zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(manager, seat);
    if (send_keymap(keyboard) < 0) {
        fprintf(stderr, "cannot create keymap\n");
        return 1;
    }
    wl_display_roundtrip(display);

    for (int i = 1; i < argc; i++) {
        char *separator = strchr(argv[i], ':');
        if (!separator) {
            fprintf(stderr, "bad action: %s\n", argv[i]);
            return 1;
        }
        *separator = '\0';
        const char *action = argv[i];
        const char *value = separator + 1;
        if (strcmp(action, "wait") == 0) {
            sleep_ms((unsigned)strtoul(value, NULL, 10));
            continue;
        }
        int code = keycode(value);
        if (code < 0)
            return 1;
        uint32_t state;
        if (strcmp(action, "down") == 0)
            state = WL_KEYBOARD_KEY_STATE_PRESSED;
        else if (strcmp(action, "up") == 0)
            state = WL_KEYBOARD_KEY_STATE_RELEASED;
        else {
            fprintf(stderr, "bad action: %s\n", action);
            return 1;
        }
        zwp_virtual_keyboard_v1_key(keyboard, now_ms(), (uint32_t)code, state);
        wl_display_flush(display);
        sleep_ms(4);
    }

    wl_display_roundtrip(display);
    zwp_virtual_keyboard_v1_destroy(keyboard);
    zwp_virtual_keyboard_manager_v1_destroy(manager);
    wl_seat_destroy(seat);
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    return 0;
}
