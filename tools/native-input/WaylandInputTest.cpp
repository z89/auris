// A private in-process Wayland server; never connects to the user's compositor.
#include <QGuiApplication>
#include <QDebug>
#include <QDir>
#include <QPointer>
#include <QProcess>
#include <QTemporaryDir>
#include <QTimer>
#include <QtWaylandCompositor/QWaylandCompositor>
#include <QtWaylandCompositor/QWaylandOutput>
#include <QtWaylandCompositor/QWaylandSurface>
#include <QtWaylandCompositor/QWaylandXdgShell>
#include <QtWaylandCompositor/QWaylandSeat>
#include <QtWaylandCompositor/QWaylandView>
#include <wayland-server-core.h>
#define namespace namespace_
#include "wlr-layer-shell-server.h"
#undef namespace

// Minimal layer-shell implementation for this test server. Only geometry is
// modeled; it has no physical display, keyboard grab, or desktop integration.
struct LayerSurface {
    wl_resource *resource;
    QPointer<QWaylandSurface> surface;
    QWaylandCompositor *compositor;
    uint32_t width = 0, height = 0, anchors = 0;
    int32_t top = 0, right = 0, bottom = 0, left = 0;
    QSize configured;
    static LayerSurface *get(wl_resource *r) {
        return static_cast<LayerSurface *>(wl_resource_get_user_data(r));
    }
    void configure() {
        const int w = (anchors & 12) == 12 ? 1000 - left - right : int(width ? width : 1000);
        const int h = (anchors & 3) == 3 ? 1000 - top - bottom : int(height ? height : 1000);
        const QSize size(w, h);
        if (size == configured) return;
        configured = size;
        zwlr_layer_surface_v1_send_configure(resource, compositor->nextSerial(), w, h);
    }
};
static const struct zwlr_layer_surface_v1_interface layerMethods = {
    [](wl_client *, wl_resource *r, uint32_t w, uint32_t h) { LayerSurface::get(r)->width = w; LayerSurface::get(r)->height = h; },
    [](wl_client *, wl_resource *r, uint32_t a) { LayerSurface::get(r)->anchors = a; },
    [](wl_client *, wl_resource *, int32_t) {},
    [](wl_client *, wl_resource *r, int32_t t, int32_t right, int32_t b, int32_t l) {
        auto *s = LayerSurface::get(r); s->top=t; s->right=right; s->bottom=b; s->left=l;
    },
    [](wl_client *, wl_resource *, uint32_t) {},
    [](wl_client *, wl_resource *, wl_resource *) {},
    [](wl_client *, wl_resource *, uint32_t) {},
    [](wl_client *, wl_resource *r) { wl_resource_destroy(r); },
    [](wl_client *, wl_resource *, uint32_t) {},
};
static const struct zwlr_layer_shell_v1_interface shellMethods = {
    [](wl_client *client, wl_resource *r, uint32_t id, wl_resource *surfaceResource,
       wl_resource *, uint32_t, const char *name) {
        auto *compositor = static_cast<QWaylandCompositor *>(wl_resource_get_user_data(r));
        auto *resource = wl_resource_create(client, &zwlr_layer_surface_v1_interface, wl_resource_get_version(r), id);
        auto *surface = QWaylandSurface::fromResource(surfaceResource);
        auto *layer = new LayerSurface{resource, surface, compositor};
        surface->setProperty("testRole", QString::fromUtf8(name));
        wl_resource_set_implementation(resource, &layerMethods, layer, [](wl_resource *r) {
            auto *layer = LayerSurface::get(r);
            if (layer->surface) QObject::disconnect(layer->surface, nullptr, layer->compositor, nullptr);
            delete layer;
        });
        QObject::connect(surface, &QWaylandSurface::redraw, compositor, [layer] { layer->configure(); });
        compositor->defaultOutput()->surfaceEnter(surface);
    },
    [](wl_client *, wl_resource *r) { wl_resource_destroy(r); },
};

int main(int argc, char **argv) {
    qputenv("QT_QPA_PLATFORM", "offscreen");
    QTemporaryDir runtime("/tmp/auris-wayland-input-XXXXXX");
    if (!runtime.isValid() || argc != 2) return 2;
    qputenv("XDG_RUNTIME_DIR", runtime.path().toUtf8());
    qunsetenv("WAYLAND_DISPLAY");
    qunsetenv("DISPLAY");
    QGuiApplication app(argc, argv);
    QWaylandCompositor compositor;
    compositor.setSocketName("auris-input-test");
    compositor.setUseHardwareIntegrationExtension(false);
    QWaylandXdgShell shell(&compositor);
    compositor.create();
    QWaylandOutput output(&compositor, nullptr);
    output.setSizeFollowsWindow(false);
    const QWaylandOutputMode mode(QSize(1000, 1000), 60000);
    output.addMode(mode, true);
    output.setCurrentMode(mode);
    compositor.setDefaultOutput(&output);
    wl_global_create(compositor.display(), &zwlr_layer_shell_v1_interface, 4, &compositor,
        [](wl_client *client, void *data, uint32_t version, uint32_t id) {
            auto *resource = wl_resource_create(client, &zwlr_layer_shell_v1_interface, int(version), id);
            wl_resource_set_implementation(resource, &shellMethods, data, nullptr);
        });
    QObject::connect(&shell, &QWaylandXdgShell::toplevelCreated, &app,
        [&](QWaylandXdgToplevel *toplevel, QWaylandXdgSurface *surface) {
            output.surfaceEnter(surface->surface());
            toplevel->sendConfigure(QSize(0, 0), QList<QWaylandXdgToplevel::State>{});
        });
    QTimer frames;
    QObject::connect(&frames, &QTimer::timeout, &app, [&] {
        for (auto *surface : compositor.surfaces()) {
            surface->frameStarted();
            surface->sendFrameCallbacks();
        }
    });
    frames.start(16);

    bool checked = false;
    bool passed = false;
    bool clickReceived = false;
    bool wheelReceived = false;
    QWaylandView pointerView;
    pointerView.setOutput(&output);
    auto check = [&] {
        checked = true;
        bool contentAccepts = false;
        bool dismissAccepts = true;
        for (auto *surface : compositor.surfaces()) {
            const bool accepts = surface->inputRegionContains(QPoint(200, 550));
            qInfo() << "COMPOSITOR" << surface->destinationSize() << "has buffer" << surface->hasContent()
                    << "accepts expanded point" << accepts;
            const QString role = surface->property("testRole").toString();
            if (surface->destinationSize().width() == 500 || role == "dms:popout") {
                contentAccepts = accepts;
                pointerView.setSurface(surface);
            }
            if (surface->destinationSize().width() == 600 || role == "dms:popout:background") dismissAccepts = accepts;
        }
        passed = contentAccepts && !dismissAccepts;
        qInfo() << (passed ? "PASS" : "FAIL") << "compositor input routing after expansion";
        if (passed) {
            auto *seat = compositor.defaultSeat();
            seat->sendMouseMoveEvent(&pointerView, QPointF(200, 550), QPointF(200, 550));
            seat->sendMousePressEvent(Qt::LeftButton);
            seat->sendMouseReleaseEvent(Qt::LeftButton);
            seat->sendMouseWheelEvent(Qt::Vertical, -120);
        }
    };
    QProcess client;
    auto env = QProcessEnvironment::systemEnvironment();
    env.insert("QT_QPA_PLATFORM", "wayland");
    env.insert("WAYLAND_DISPLAY", "auris-input-test");
    env.insert("QT_QUICK_BACKEND", "software");
    env.remove("QSG_RHI_BACKEND");
    env.insert("QT_WAYLAND_DISABLE_WINDOWDECORATION", "1");
    env.insert("QML_IMPORT_PATH", QString::fromLocal8Bit(argv[1]));
    env.insert("AURIS_NATIVE_WAYLAND", "1");
    client.setProcessEnvironment(env);
    client.setProcessChannelMode(QProcess::MergedChannels);
    QByteArray pending;
    QObject::connect(&client, &QProcess::readyReadStandardOutput, &app, [&] {
        pending += client.readAllStandardOutput();
        for (int newline; (newline = pending.indexOf('\n')) >= 0;) {
            const auto line = pending.left(newline);
            pending.remove(0, newline + 1);
            qInfo().noquote() << line;
            if (line.contains("expanded hit")) QTimer::singleShot(50, &app, check);
            if (line.contains("CONTENT CLICK")) clickReceived = true;
            if (line.contains("CONTENT WHEEL")) wheelReceived = true;
        }
    });
    QObject::connect(&client, qOverload<int, QProcess::ExitStatus>(&QProcess::finished), &app,
        [&](int code, QProcess::ExitStatus status) {
            qInfo() << "pointer events: click" << clickReceived << "wheel" << wheelReceived;
            app.exit(checked && passed && clickReceived && wheelReceived && code == 0 && status == QProcess::NormalExit ? 0 : 1);
        });
    QTimer::singleShot(8000, &app, [&] {
        qCritical() << "TIMEOUT";
        client.kill(); // Only the child connected to this private test socket.
        client.waitForFinished();
        app.exit(2);
    });
    client.start("/usr/bin/qs", {"-p", QDir(QString::fromLocal8Bit(argv[1])).filePath("shell.qml")});
    return app.exec();
}
