// Regression for adding a plugin component while the QML engine is running.
// Everything is confined to QTemporaryDir and test-only DMS widget stubs.
#include <QDir>
#include <QFile>
#include <QGuiApplication>
#include <QQmlComponent>
#include <QQmlEngine>
#include <QTemporaryDir>
#include <QDebug>
#include <memory>
#include <cstdio>

int main(int argc, char **argv)
{
    QGuiApplication app(argc, argv);
    if (argc != 2)
        return 1;
    const QDir repo(QString::fromLocal8Bit(argv[1]));
    QTemporaryDir scratch(QDir::tempPath() + "/auris-late-qml-XXXXXX");
    if (!scratch.isValid())
        return 1;
    const QDir plugin(scratch.path());
    QFile warmFile(plugin.filePath("Existing.qml"));
    if (!warmFile.open(QIODevice::WriteOnly))
        return 1;
    warmFile.write("import QtQml\nQtObject {}\n");
    warmFile.close();

    QQmlEngine engine;
    engine.addImportPath(repo.filePath("tools/qml-stubs"));
    QQmlComponent warm(&engine, QUrl::fromLocalFile(warmFile.fileName()));
    if (!warm.isReady()) {
        qCritical().noquote() << warm.errorString();
        return 1;
    }

    const QString settings = repo.filePath("components/settings/AurisAdvancedSettings.qml");
    const QString late = plugin.filePath("AurisAdvancedSettings.qml");
    if (!QFile::copy(settings, late))
        return 1;
    QUrl lateUrl = QUrl::fromLocalFile(late);
    lateUrl.setQuery("retry=1");
    QQmlComponent broken(&engine, lateUrl);
    if (!broken.isError() || !broken.errorString().contains("File name case mismatch")) {
        qCritical() << "Expected stale-directory failure:" << broken.errorString();
        return 1;
    }

    // A timestamp changes the component key, not the directory-listing cache.
    lateUrl.setQuery("retry=2");
    QQmlComponent retried(&engine, lateUrl);
    if (!retried.isError() || !retried.errorString().contains("File name case mismatch"))
        return 1;

    if (!plugin.mkpath("components/settings"))
        return 1;
    const QString isolated = plugin.filePath("components/settings/AurisAdvancedSettings.qml");
    if (!QFile::copy(settings, isolated))
        return 1;
    QQmlComponent fixed(&engine, QUrl::fromLocalFile(isolated));
    if (!fixed.isReady()) {
        qCritical().noquote() << fixed.errorString();
        return 1;
    }
    std::unique_ptr<QObject> panel(fixed.createWithInitialProperties({{"width", 420}}));
    if (!panel || panel->property("implicitHeight").toDouble() <= 0) {
        qCritical().noquote() << fixed.errorString();
        return 1;
    }
    std::puts("PASS: reproduced stale-directory failure and retry failure;"
              " isolated settings directory loads in the same engine");
    return 0;
}
