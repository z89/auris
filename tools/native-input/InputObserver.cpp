// Read-only native-window instrumentation. Does not synthesize or consume input.
#include <QElapsedTimer>
#include <QEvent>
#include <QGuiApplication>
#include <QHash>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMouseEvent>
#include <QPointer>
#include <QQmlExtensionPlugin>
#include <QQuickItem>
#include <QQuickWindow>
#include <QRegion>

class InputObserver : public QObject {
    Q_OBJECT
public:
    using QObject::QObject;
    Q_INVOKABLE void watch(QQuickItem *item, const QString &label) {
        QQuickWindow *window = item ? item->window() : nullptr;
        if (!window || windows.contains(window)) return;
        windows.insert(window, label);
        window->installEventFilter(this);
        connect(window, &QObject::destroyed, this, [this, window] { windows.remove(window); });
        snapshot("watch-" + label);
    }
    Q_INVOKABLE void snapshot(const QString &reason) const {
        QJsonArray all;
        for (auto it = windows.cbegin(); it != windows.cend(); ++it) {
            auto *window = it.key();
            QJsonArray rects;
            for (const QRect &rect : window->mask())
                rects.append(QJsonArray{rect.x(), rect.y(), rect.width(), rect.height()});
            all.append(QJsonObject{{"window", it.value()}, {"visible", window->isVisible()},
                {"geometry", QJsonArray{window->x(), window->y(), window->width(), window->height()}},
                {"transparentForInput", window->flags().testFlag(Qt::WindowTransparentForInput)},
                {"active", window->isActive()}, {"mask", rects}});
        }
        qInfo().noquote() << "auris: native-input" << reason << QJsonDocument(all).toJson(QJsonDocument::Compact);
    }
protected:
    bool eventFilter(QObject *object, QEvent *event) override {
        auto *window = qobject_cast<QQuickWindow *>(object);
        const auto type = event->type();
        if (type == QEvent::MouseButtonPress || type == QEvent::MouseButtonRelease ||
            type == QEvent::Wheel || type == QEvent::Enter || type == QEvent::Leave ||
            (type == QEvent::MouseMove && (!lastMove.isValid() || lastMove.elapsed() >= 200))) {
            if (type == QEvent::MouseMove) lastMove.restart();
            auto *pointEvent = dynamic_cast<QSinglePointEvent *>(event);
            const QPointF pos = pointEvent ? pointEvent->position() : QPointF(-1, -1);
            const QPointF global = pointEvent ? pointEvent->globalPosition() : QPointF(-1, -1);
            const QJsonObject record{{"window", windows.value(window)}, {"event", int(type)},
                {"local", QJsonArray{pos.x(), pos.y()}}, {"global", QJsonArray{global.x(), global.y()}}};
            qInfo().noquote() << "auris: native-event" << QJsonDocument(record).toJson(QJsonDocument::Compact);
            if (type != QEvent::MouseMove) snapshot("event");
        }
        return false;
    }
private:
    QHash<QQuickWindow *, QString> windows;
    QElapsedTimer lastMove;
};

class InputObserverPlugin : public QQmlExtensionPlugin {
    Q_OBJECT
    Q_PLUGIN_METADATA(IID QQmlExtensionInterface_iid)
public:
    void registerTypes(const char *uri) override {
        qmlRegisterType<InputObserver>(uri, 1, 0, "InputObserver");
    }
};
#include "InputObserver.moc"
