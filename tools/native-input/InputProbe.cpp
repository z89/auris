// Used only by the isolated Quickshell input-region regression.
#include <QBitArray>
#include <QTest>
#include <QQmlExtensionPlugin>
#include <QGuiApplication>
#include <QQuickItem>
#include <QQuickWindow>
#include <QRegion>

class InputProbe : public QObject {
    Q_OBJECT
public:
    using QObject::QObject;
    Q_INVOKABLE bool accepts(QQuickItem *item, int x, int y) const {
        auto *window = item ? item->window() : nullptr;
        if (!window || !window->isVisible()) return false;
        if (window->flags().testFlag(Qt::WindowTransparentForInput)) return false;
        return window->mask().isEmpty() || window->mask().contains(QPoint(x, y));
    }
    Q_INVOKABLE QString describe(QQuickItem *item) const {
        auto *window = item ? item->window() : nullptr;
        if (!window) return QStringLiteral("no window");
        QString description;
        QDebug stream(&description);
        stream << window->size() << window->isVisible() << window->mask()
               << window->flags().testFlag(Qt::WindowTransparentForInput);
        return description;
    }
    Q_INVOKABLE void click(QQuickItem *item, int x, int y) const {
        // Hard fail before any synthetic event if launched against a desktop.
        if (QGuiApplication::platformName() != QStringLiteral("offscreen"))
            qFatal("Input probe requires QT_QPA_PLATFORM=offscreen");
        if (!item || !item->window()) qFatal("Input probe has no window");
        QTest::mouseMove(item->window(), QPoint(x, y));
        QTest::mouseClick(item->window(), Qt::LeftButton, Qt::NoModifier, QPoint(x, y));
    }
};

class InputProbePlugin : public QQmlExtensionPlugin {
    Q_OBJECT
    Q_PLUGIN_METADATA(IID QQmlExtensionInterface_iid)
public:
    void registerTypes(const char *uri) override {
        qmlRegisterType<InputProbe>(uri, 1, 0, "InputProbe");
    }
};

#include "InputProbe.moc"
