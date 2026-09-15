// Run UI regressions with DMS's actual button, hover-delay and tooltip code.
// All session services, IO and theme inputs remain private test doubles.
#include <QCoreApplication>
#include <QDir>
#include <QFile>
#include <QProcess>
#include <QProcessEnvironment>
#include <QTemporaryDir>
#include <QDebug>

static QByteArray read(const QString &path)
{
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly))
        qFatal("Cannot read %s", qPrintable(path));
    return file.readAll();
}

static void write(const QString &path, const QByteArray &body)
{
    QFile file(path);
    if (!file.open(QIODevice::WriteOnly) || file.write(body) != body.size())
        qFatal("Cannot write fixture %s", qPrintable(path));
}

static void copyTree(const QDir &source, const QDir &target)
{
    if (!QDir().mkpath(target.path()))
        qFatal("Cannot create fixture directory");
    for (const QFileInfo &entry : source.entryInfoList(QDir::Files | QDir::Dirs | QDir::NoDotAndDotDot)) {
        if (entry.isDir())
            copyTree(QDir(entry.filePath()), QDir(target.filePath(entry.fileName())));
        else
            write(target.filePath(entry.fileName()), read(entry.filePath()));
    }
}

int main(int argc, char **argv)
{
    QCoreApplication app(argc, argv);
    if (argc != 3) {
        qCritical() << "Usage: real-dms-widgets-test REPO DMS_SOURCE";
        return 1;
    }
    const QDir repo(QString::fromLocal8Bit(argv[1]));
    const QDir dms(QString::fromLocal8Bit(argv[2]));
    QTemporaryDir scratch(QDir::tempPath() + "/auris-real-widgets-XXXXXX");
    if (!scratch.isValid())
        return 1;
    const QDir fixture(scratch.path());
    copyTree(QDir(repo.filePath("tools/qml-stubs")), fixture);
    const QDir common(fixture.filePath("qs/DankCommon/Widgets"));
    copyTree(QDir(repo.filePath("tools/qml-stubs/qs/Widgets")), common);
    QByteArray module = read(common.filePath("qmldir"));
    module.replace("module qs.Widgets", "module qs.DankCommon.Widgets");
    for (const char *name : {"DankButton", "DankActionButton", "StateLayer", "DankTooltipV2"}) {
        const QString type = QString::fromLatin1(name);
        QByteArray source = read(dms.filePath("DankCommon/Widgets/" + type + ".qml"));
        if (type == "DankTooltipV2")
            source.replace("id: tooltip", "id: tooltip\n        objectName: \"realDmsHoverTooltip\"");
        write(common.filePath(type + ".qml"), source);
        if (!module.contains(type.toUtf8() + " 1.0"))
            module += type.toUtf8() + " 1.0 " + type.toUtf8() + ".qml\n";
    }
    for (const char *name : {"DankButton", "DankActionButton"}) {
        const QByteArray type(name);
        write(fixture.filePath("qs/Widgets/" + QString::fromLatin1(name) + ".qml"),
              "import qs.DankCommon.Widgets as Real\nReal." + type + " {}\n");
    }
    // Pure visuals unrelated to button geometry/tooltip ownership are inert.
    write(common.filePath("FocusRing.qml"), "import QtQuick\nItem {}\n");
    write(common.filePath("DankColorAnim.qml"), "import QtQuick\nColorAnimation {}\n");
    write(common.filePath("DankRipple.qml"), "import QtQuick\nItem { property color rippleColor; property real cornerRadius; property bool enableRipple; function trigger(x,y) {} }\n");
    module += "FocusRing 1.0 FocusRing.qml\nDankColorAnim 1.0 DankColorAnim.qml\nDankRipple 1.0 DankRipple.qml\n";
    write(common.filePath("qmldir"), module);
    QDir().mkpath(fixture.filePath("qs/DankCommon/Common"));
    QByteArray style = read(fixture.filePath("qs/Common/Theme.qml"));
    style.replace("QtObject {", "QtObject {\n    enum AnimationSpeed { None, Normal }\n    property int currentAnimationSpeed: 0\n    property bool enableRippleEffects: false\n    property int shorterDuration: 1\n    property var expressiveCurves: ({ standardDecel: [0,0,1,1,1,1], standard: [0,0,1,1,1,1] })\n");
    write(fixture.filePath("qs/DankCommon/Common/Style.qml"), style);
    write(fixture.filePath("qs/DankCommon/Common/qmldir"), "module qs.DankCommon.Common\nsingleton Style 1.0 Style.qml\n");

    QProcess runner;
    auto env = QProcessEnvironment::systemEnvironment();
    env.insert("QT_QPA_PLATFORM", "offscreen");
    env.insert("QSG_RHI_BACKEND", "software");
    env.insert("QT_FORCE_STDERR_LOGGING", "1");
    runner.setProcessEnvironment(env);
    runner.setProcessChannelMode(QProcess::ForwardedChannels);
    runner.start("/usr/lib/qt6/bin/qmltestrunner", {"-input", repo.filePath("tools/tst_AurisUiRegression.qml"), "-import", fixture.path()});
    if (!runner.waitForStarted() || !runner.waitForFinished(90000)) {
        runner.kill(); // Only this test-owned, offscreen child process.
        runner.waitForFinished();
        return 1;
    }
    return runner.exitStatus() == QProcess::NormalExit ? runner.exitCode() : 1;
}
