// Same-engine, same-file revision reload. No session IO or live desktop.
#include <QCoreApplication>
#include <QDebug>
#include <QDir>
#include <QFile>
#include <QQmlComponent>
#include <QQmlEngine>
#include <QTemporaryDir>
#include <memory>

static bool writeRevision(const QString &path, int revision)
{
    QFile file(path);
    if (!file.open(QIODevice::WriteOnly | QIODevice::Truncate))
        return false;
    const QByteArray source = "import QtQml\nQtObject { property int revision: "
        + QByteArray::number(revision) + " }\n";
    return file.write(source) == source.size();
}

int main(int argc, char **argv)
{
    QCoreApplication app(argc, argv);
    QTemporaryDir scratch(QDir::tempPath() + "/auris-reload-qml-XXXXXX");
    if (!scratch.isValid())
        return 1;
    const QString path = scratch.filePath("Widget.qml");
    if (!writeRevision(path, 1))
        return 1;
    QQmlEngine engine;
    QUrl url = QUrl::fromLocalFile(path);
    url.setQuery("t=1");
    QQmlComponent first(&engine, url);
    std::unique_ptr<QObject> oldInstance(first.create());
    if (!oldInstance || oldInstance->property("revision").toInt() != 1) {
        qCritical().noquote() << first.errorString();
        return 1;
    }
    if (!writeRevision(path, 2))
        return 1;
    url.setQuery("t=2");
    QQmlComponent second(&engine, url);
    std::unique_ptr<QObject> newInstance(second.create());
    if (!newInstance || newInstance->property("revision").toInt() != 2) {
        qCritical() << "Fresh URL did not load revision 2:" << second.errorString()
                    << (newInstance ? newInstance->property("revision") : QVariant());
        return 1;
    }
    if (oldInstance->property("revision").toInt() != 1)
        return 1;
    qInfo() << "PASS: a fresh query loads changed source in the same Qt engine;"
               " existing objects retain their original revision";
    return 0;
}
