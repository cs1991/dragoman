using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;
using System.Web.Script.Serialization;
using System.Windows.Forms;

internal sealed class DragomanGui : Form
{
    public sealed class Account
    {
        public string id { get; set; }
        public string name { get; set; }
        public bool enabled { get; set; }
        public string model { get; set; }
        public string proxy { get; set; }
        public string local_ip { get; set; }
    }

    private readonly string executable = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "dragoman.exe");
    private readonly ListView accounts = new ListView();
    private readonly Label status = new Label { AutoSize = true };
    private readonly Label message = new Label { Dock = DockStyle.Fill, AutoEllipsis = true };
    private readonly NumericUpDown port = new NumericUpDown { Minimum = 1, Maximum = 65535, Value = 8080, Width = 78 };
    private readonly NumericUpDown share = new NumericUpDown { Minimum = 1, Maximum = 100, Value = 1, Width = 52 };
    private readonly ComboBox strategy = new ComboBox { DropDownStyle = ComboBoxStyle.DropDownList, Width = 120 };
    private readonly Button start, stop, copy, add, batchLogin, loginAgain, toggle, remove, setProxy, bulkProxy, refresh, cancelLogin;
    private Process server, login;
    private bool busy, ready, closing, stopping, loginCanceled;
    private Account Selected { get { return accounts.SelectedItems.Count == 0 ? null : (Account)accounts.SelectedItems[0].Tag; } }
    private string Endpoint { get { return "http://127.0.0.1:" + port.Value + "/v1"; } }

    [STAThread]
    private static void Main()
    {
        bool created;
        using (var mutex = new Mutex(true, @"Local\DragomanDesktop", out created))
        {
            if (!created)
            {
                MessageBox.Show("Dragoman 桌面版已打开。", "Dragoman");
                return;
            }
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            Application.Run(new DragomanGui());
        }
    }

    private DragomanGui()
    {
        Text = "Dragoman";
        Font = new Font("Microsoft YaHei UI", 10);
        StartPosition = FormStartPosition.CenterScreen;
        ClientSize = new Size(880, 470);
        MinimumSize = new Size(820, 400);

        var layout = new TableLayoutPanel { Dock = DockStyle.Fill, Padding = new Padding(16), ColumnCount = 1, RowCount = 6 };
        layout.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.Absolute, 48));
        Controls.Add(layout);
        layout.Controls.Add(new Label { Text = "Dragoman  |  本地 API 代理", AutoSize = true, Font = new Font(Font.FontFamily, 16), Margin = new Padding(3, 2, 3, 10) }, 0, 0);
        var serverBar = Bar(layout, 1);
        serverBar.Controls.Add(Caption("端口"));
        serverBar.Controls.Add(port);
        serverBar.Controls.Add(Caption("账号策略"));
        strategy.Items.AddRange(new object[] { "轮询", "最少使用" });
        strategy.SelectedIndex = 0;
        serverBar.Controls.Add(strategy);
        serverBar.Controls.Add(Caption("每 IP 账号"));
        serverBar.Controls.Add(share);
        start = Button(serverBar, "启动", async () => await Run(StartProxy));
        stop = Button(serverBar, "停止", async () => await Run(StopProxy));
        copy = Button(serverBar, "复制 API 地址", CopyEndpoint);
        status.Margin = new Padding(10, 7, 0, 0);
        serverBar.Controls.Add(status);

        layout.Controls.Add(Caption("已保存的 Gemini 账号"), 0, 2);
        accounts.Dock = DockStyle.Fill;
        accounts.View = View.Details;
        accounts.FullRowSelect = true;
        accounts.MultiSelect = false;
        accounts.HideSelection = false;
        accounts.Columns.Add("账号", 210);
        accounts.Columns.Add("状态", 130);
        accounts.Columns.Add("模型", 150);
        accounts.Columns.Add("代理", 300);
        accounts.SelectedIndexChanged += (s, e) => UpdateButtons();
        layout.Controls.Add(accounts, 0, 3);
        var accountBar = Bar(layout, 4);
        add = Button(accountBar, "新增账号", async () => await Run(AddAccount));
        batchLogin = Button(accountBar, "批量登录", async () => await Run(BatchLogin));
        loginAgain = Button(accountBar, "重新登录", async () => await Run(() => LogIn(Selected.id, Selected.proxy)));
        setProxy = Button(accountBar, "设置代理", async () => await Run(EditProxy));
        bulkProxy = Button(accountBar, "批量分配代理", async () => await Run(AssignProxies));
        toggle = Button(accountBar, "停用", async () => await Run(ToggleAccount));
        remove = Button(accountBar, "删除", async () => await Run(RemoveAccount));
        refresh = Button(accountBar, "刷新列表", async () => await Run(RefreshAccounts));
        cancelLogin = Button(accountBar, "取消登录", () => { loginCanceled = true; TryKill(login); });
        layout.Controls.Add(message, 0, 5);
        Shown += async (s, e) => await Run(RefreshAccounts);
        FormClosing += OnClosing;
        UpdateButtons();
        message.Text = "账号更改前请停止代理。登录成功后账号自动添加，Chrome 窗口保持打开。";
    }

    private static FlowLayoutPanel Bar(TableLayoutPanel layout, int row)
    {
        var bar = new FlowLayoutPanel { Dock = DockStyle.Fill, AutoSize = true, WrapContents = true };
        layout.Controls.Add(bar, 0, row);
        return bar;
    }

    private static Label Caption(string text)
    {
        return new Label { Text = text, AutoSize = true, Margin = new Padding(3, 7, 5, 7) };
    }

    private Button Button(FlowLayoutPanel bar, string text, Action action)
    {
        var button = new Button { Text = text, AutoSize = true };
        button.Click += (s, e) => action();
        bar.Controls.Add(button);
        return button;
    }

    private void UpdateButtons()
    {
        var selected = Selected;
        bool edit = !busy && server == null && !closing;
        add.Enabled = batchLogin.Enabled = port.Enabled = strategy.Enabled = share.Enabled = edit;
        remove.Enabled = toggle.Enabled = loginAgain.Enabled = setProxy.Enabled = edit && selected != null;
        bulkProxy.Enabled = edit;
        toggle.Text = selected != null && !selected.enabled ? "启用" : "停用";
        start.Enabled = edit;
        stop.Enabled = server != null && !busy && !closing;
        copy.Enabled = ready;
        refresh.Enabled = !busy && !closing;
        cancelLogin.Enabled = login != null && !closing;
        status.Text = server == null ? "● 已停止" : stopping ? "● 正在停止" : ready ? "● 运行中" : "● 正在启动";
        status.ForeColor = ready ? Color.SeaGreen : SystemColors.ControlText;
    }

    private void CopyEndpoint()
    {
        try
        {
            Clipboard.SetText(Endpoint);
            message.Text = "已复制  " + Endpoint;
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            message.Text = "剪贴板暂时不可用。API 地址  " + Endpoint;
        }
    }

    private async Task Run(Func<Task> action)
    {
        if (busy || closing) return;
        busy = true;
        UpdateButtons();
        try { await action(); }
        catch (Exception error) { message.Text = error.Message; }
        finally { busy = false; if (!IsDisposed) UpdateButtons(); }
    }

    // Account IDs contain only the validated identifier characters emitted by Rust.
    private static string AccountArg(string id)
    {
        if (!Regex.IsMatch(id, @"\A(?:legacy|named:[a-zA-Z0-9._-]+)\z"))
            throw new InvalidOperationException("账号标识无效。");
        return id;
    }

    private Process Spawn(string arguments, bool redirectInput = false)
    {
        return Process.Start(new ProcessStartInfo(executable, arguments) {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = redirectInput,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8
        });
    }

    private static async Task<string> ReadResult(Process child, string failure)
    {
        var output = child.StandardOutput.ReadToEndAsync();
        var error = child.StandardError.ReadToEndAsync();
        await Task.Run(() => child.WaitForExit());
        // Chrome inherits the redirected pipe handles and keeps them open after
        // dragoman.exe exits, so waiting for stream end would hang the dialog.
        await Task.WhenAny(Task.WhenAll(output, error), Task.Delay(1500));
        if (child.ExitCode != 0) throw new InvalidOperationException(failure);
        return output.Status == TaskStatus.RanToCompletion ? output.Result : string.Empty;
    }

    private async Task<string> Manage(string arguments)
    {
        using (var child = Spawn("desktop-accounts " + arguments))
            return await ReadResult(child, "账号操作失败。请刷新列表，或检查配置目录权限。");
    }

    private async Task RefreshAccounts()
    {
        var entries = new JavaScriptSerializer().Deserialize<Account[]>(await Manage("list"));
        var previous = Selected;
        accounts.BeginUpdate();
        accounts.Items.Clear();
        foreach (var account in entries)
        {
            var row = new ListViewItem(account.name) { Tag = account };
            row.SubItems.Add((account.enabled ? "已启用" : "已停用") + (account.model == null ? " / 需登录" : ""));
            row.SubItems.Add(account.model ?? "—");
            var route = String.IsNullOrEmpty(account.proxy) ? "系统" : account.proxy;
            if (!String.IsNullOrEmpty(account.local_ip)) route += "  @ " + account.local_ip;
            row.SubItems.Add(route);
            accounts.Items.Add(row);
            row.Selected = previous != null && account.id == previous.id && account.enabled == previous.enabled;
        }
        accounts.EndUpdate();
    }

    private static bool ValidProxy(string value)
    {
        return Regex.IsMatch(value,
            @"\A(?:https?://)?(?:[^@\s/]+@)?[A-Za-z0-9._-]+:\d{1,5}\z",
            RegexOptions.IgnoreCase);
    }

    private string NextProxy()
    {
        return "http://127.0.0.1:7897";
    }

    private async Task AddAccount()
    {
        using (var dialog = new Form { Text = "新增账号", ClientSize = new Size(430, 215), Font = Font,
            StartPosition = FormStartPosition.CenterParent, FormBorderStyle = FormBorderStyle.FixedDialog,
            MinimizeBox = false, MaximizeBox = false, ShowInTaskbar = false })
        {
            var prompt = new Label { Text = "账号别名（字母、数字、点、横线或下划线）", AutoSize = true, Location = new Point(12, 12) };
            var name = new TextBox { Bounds = new Rectangle(12, 40, 400, 28), MaxLength = 64 };
            var proxyPrompt = new Label { Text = "出口代理，固定填 Clash Verge 混合端口 http://127.0.0.1:7897", AutoSize = true, Location = new Point(12, 78) };
            var proxy = new TextBox { Bounds = new Rectangle(12, 106, 400, 28), MaxLength = 200, Text = NextProxy() };
            var ok = new Button { Text = "打开 Chrome", AutoSize = true, Location = new Point(12, 160) };
            ok.Click += (s, e) => {
                var value = name.Text.Trim();
                if (!Regex.IsMatch(value, @"\A[a-zA-Z0-9_-][a-zA-Z0-9._-]*\z"))
                    MessageBox.Show(dialog, "请输入有效的账号别名。", "Dragoman");
                else if (accounts.Items.Cast<ListViewItem>().Any(row =>
                    String.Equals(((Account)row.Tag).id, "named:" + value, StringComparison.OrdinalIgnoreCase)))
                    MessageBox.Show(dialog, "此别名已存在。请选择该账号并重新登录。", "Dragoman");
                else if (proxy.Text.Trim().Length > 0 && !ValidProxy(proxy.Text.Trim()))
                    MessageBox.Show(dialog, "代理地址格式为 host:port 或 http://host:port。", "Dragoman");
                else dialog.DialogResult = DialogResult.OK;
            };
            dialog.Controls.AddRange(new Control[] { prompt, name, proxyPrompt, proxy, ok });
            dialog.AcceptButton = ok;
            if (dialog.ShowDialog(this) == DialogResult.OK)
                await LogIn("named:" + name.Text.Trim(), proxy.Text.Trim());
        }
    }

    private async Task EditProxy()
    {
        var account = Selected;
        using (var dialog = new Form { Text = "设置代理", ClientSize = new Size(430, 215), Font = Font,
            StartPosition = FormStartPosition.CenterParent, FormBorderStyle = FormBorderStyle.FixedDialog,
            MinimizeBox = false, MaximizeBox = false, ShowInTaskbar = false })
        {
            var prompt = new Label { Text = "出口代理。固定 http://127.0.0.1:7897。", AutoSize = true, Location = new Point(12, 12) };
            var proxy = new TextBox { Bounds = new Rectangle(12, 40, 400, 28), MaxLength = 200, Text = account.proxy ?? "" };
            var ipPrompt = new Label { Text = "源地址。每个账号一个，如 127.0.0.7。", AutoSize = true, Location = new Point(12, 76) };
            var localIp = new TextBox { Bounds = new Rectangle(12, 104, 400, 28), MaxLength = 40, Text = account.local_ip ?? "" };
            var ok = new Button { Text = "保存", AutoSize = true, Location = new Point(12, 150) };
            ok.Click += (s, e) => {
                var value = proxy.Text.Trim();
                if (value.Length > 0 && !ValidProxy(value))
                    MessageBox.Show(dialog, "代理地址格式为 host:port 或 http://host:port。", "Dragoman");
                else dialog.DialogResult = DialogResult.OK;
            };
            dialog.Controls.AddRange(new Control[] { prompt, proxy, ipPrompt, localIp, ok });
            dialog.AcceptButton = ok;
            if (dialog.ShowDialog(this) == DialogResult.OK)
            {
                await Manage("set-proxy --id=" + AccountArg(account.id)
                    + " --proxy=" + proxy.Text.Trim()
                    + " --local-ip=" + localIp.Text.Trim());
                await RefreshAccounts();
                message.Text = proxy.Text.Trim().Length == 0
                    ? "已清空该账号的代理。"
                    : "已保存该账号的出口，重启代理后生效。";
            }
        }
    }

    private async Task AssignProxies()
    {
        using (var dialog = new Form { Text = "批量分配代理", ClientSize = new Size(560, 230), Font = Font,
            StartPosition = FormStartPosition.CenterParent, FormBorderStyle = FormBorderStyle.FixedDialog,
            MinimizeBox = false, MaximizeBox = false, ShowInTaskbar = false })
        {
            var startPrompt = new Label { Text = "起始源地址 127.0.0.x", AutoSize = true, Location = new Point(12, 12) };
            var startIp = new NumericUpDown { Minimum = 1, Maximum = 254, Value = 2, Bounds = new Rectangle(12, 40, 110, 28) };
            var sharePrompt = new Label { Text = "每个 IP 承载账号数", AutoSize = true, Location = new Point(150, 12) };
            var perPort = new NumericUpDown { Minimum = 1, Maximum = 100, Value = share.Value, Bounds = new Rectangle(150, 40, 110, 28) };
            var portPrompt = new Label { Text = "代理端口", AutoSize = true, Location = new Point(288, 12) };
            var proxyPort = new NumericUpDown { Minimum = 1, Maximum = 65535, Value = 7897, Bounds = new Rectangle(288, 40, 110, 28) };
            var note = new Label { Text = "账号按名称排序，前 N 个账号使用起始源地址，之后加 1。全部走 Clash 的同一个混合端口，靠源 IP 选节点。", AutoSize = false, Bounds = new Rectangle(12, 90, 520, 36) };
            var ok = new Button { Text = "分配", AutoSize = true, Location = new Point(12, 140) };
            ok.Click += (s, e) => dialog.DialogResult = DialogResult.OK;
            dialog.Controls.AddRange(new Control[] { startPrompt, startIp, sharePrompt, perPort, portPrompt, proxyPort, note, ok });
            dialog.AcceptButton = ok;
            if (dialog.ShowDialog(this) != DialogResult.OK) return;
            await Manage("assign-proxies --start-ip=" + startIp.Value
                + " --proxy-port=" + proxyPort.Value
                + " --per-ip=" + perPort.Value);
            share.Value = perPort.Value;
            await RefreshAccounts();
            message.Text = "已分配源地址。重启代理后生效。";
        }
    }

    private async Task LogIn(string id, string proxy)
    {
        loginCanceled = false;
        var arguments = "login --desktop-id=" + AccountArg(id);
        if (!String.IsNullOrEmpty(proxy))
        {
            if (!ValidProxy(proxy)) throw new InvalidOperationException("代理地址格式为 host:port 或 http://host:port。");
            arguments += " --proxy=" + proxy;
        }
        login = Spawn(arguments);
        message.Text = "Chrome 正在打开。完成 Google 登录后账号自动添加，窗口保持打开。";
        UpdateButtons();
        try
        {
            await ReadLogin(login);
            message.Text = "账号已添加。Chrome 窗口保持打开，关闭与否都不影响使用。";
            await RefreshAccounts();
        }
        catch (InvalidOperationException)
        {
            if (!loginCanceled) throw;
            message.Text = "已取消登录。Chrome 窗口保持打开。";
        }
        finally { login.Dispose(); login = null; }
    }

    private async Task ReadLogin(Process child)
    {
        var output = Task.Run(async () =>
        {
            string line;
            while ((line = await child.StandardOutput.ReadLineAsync()) != null)
            {
                var text = LoginProgress(line);
                if (text == null || IsDisposed || !IsHandleCreated) continue;
                try { BeginInvoke(new Action(() => { if (!IsDisposed) message.Text = text; })); }
                catch (InvalidOperationException) { }
            }
        });
        var error = child.StandardError.ReadToEndAsync();
        await Task.Run(() => child.WaitForExit());
        // Chrome inherits the redirected pipe handles and keeps them open after
        // dragoman.exe exits, so waiting for stream end would hang the dialog.
        await Task.WhenAny(Task.WhenAll(output, error), Task.Delay(1500));
        if (child.ExitCode != 0)
        {
            var text = "登录未完成。请确认已安装 Chrome，并保持登录窗口打开后重试。";
            if (error.Status == TaskStatus.RanToCompletion)
            {
                var tail = error.Result.Split('\n').Select(value => value.Trim())
                    .LastOrDefault(value => value.Length > 0);
                if (!String.IsNullOrEmpty(tail))
                    text = tail.StartsWith("Error: ", StringComparison.Ordinal) ? tail.Substring(7) : tail;
            }
            throw new InvalidOperationException(text);
        }
    }

    private static string LoginProgress(string line)
    {
        if (line.StartsWith("SUCCESS", StringComparison.Ordinal)) return "账号已添加。Chrome 窗口保持打开。";
        if (line.Contains("已检测到登录状态")) return "已检测到登录状态，正在读取会话…";
        if (line.Contains("已识别登录")) return "已识别登录状态，正在保存账号…";
        if (line.Contains("监听登录状态")) return "正在监听登录状态。完成 Google 登录后账号会自动添加。";
        return null;
    }

    private async Task BatchLogin()
    {
        using (var dialog = new Form
        {
            Text = "批量登录",
            ClientSize = new Size(620, 460),
            MinimumSize = new Size(520, 360),
            StartPosition = FormStartPosition.CenterParent,
            Font = Font
        })
        {
            var hint = new Label
            {
                Dock = DockStyle.Top,
                Height = 84,
                Padding = new Padding(4, 6, 4, 6),
                Text = "每行一个账号：邮箱----密码--- 或 邮箱----密码----2FA密钥。可粘贴，也可从文件导入。"
                    + "换行支持 CRLF、LF、CR，编码支持 UTF-8 与 UTF-16。"
                    + "验证码优先用账号自带的 2FA 密钥生成，没有密钥时从 google111111.asia 读取。"
            };
            var box = new TextBox
            {
                Multiline = true,
                Dock = DockStyle.Fill,
                ScrollBars = ScrollBars.Both,
                AcceptsReturn = true,
                WordWrap = false,
                Font = new Font("Consolas", 9)
            };
            var bottom = new FlowLayoutPanel
            {
                Dock = DockStyle.Bottom,
                FlowDirection = FlowDirection.RightToLeft,
                AutoSize = true,
                Padding = new Padding(0, 8, 0, 0)
            };
            var ok = new Button { Text = "开始登录", AutoSize = true, DialogResult = DialogResult.OK };
            var cancel = new Button { Text = "取消", AutoSize = true, DialogResult = DialogResult.Cancel };
            var import = new Button { Text = "导入文件", AutoSize = true };
            import.Click += (s, e) =>
            {
                using (var pick = new OpenFileDialog
                {
                    Filter = "文本文件 (*.txt)|*.txt|所有文件 (*.*)|*.*",
                    Title = "导入账号文件"
                })
                {
                    if (pick.ShowDialog(dialog) != DialogResult.OK) return;
                    try { box.Text = NormalizeAccounts(ReadAccountFile(pick.FileName)); }
                    catch (Exception error)
                    {
                        MessageBox.Show(dialog, error.Message, "Dragoman");
                    }
                }
            };
            bottom.Controls.Add(cancel);
            bottom.Controls.Add(ok);
            bottom.Controls.Add(import);
            dialog.Controls.Add(box);
            dialog.Controls.Add(bottom);
            dialog.Controls.Add(hint);
            dialog.AcceptButton = ok;
            dialog.CancelButton = cancel;

            if (dialog.ShowDialog(this) != DialogResult.OK) return;
            var text = NormalizeAccounts(box.Text).Trim();
            if (text.Length == 0) throw new InvalidOperationException("请粘贴或导入账号。");
            var path = Path.Combine(Path.GetTempPath(), "dragoman-batch-" + Guid.NewGuid().ToString("N") + ".txt");
            File.WriteAllText(path, text, new UTF8Encoding(false));
            try { await RunBatchFile(path); }
            finally
            {
                try { File.Delete(path); }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
            }
        }
    }

    private static readonly Regex MailboxPattern = new Regex(
        @"[A-Za-z0-9._%+]+(?:-[A-Za-z0-9._%+]+)*@(?:[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*\.)+[A-Za-z]{2,}",
        RegexOptions.Compiled);

    private static string ReadAccountFile(string path)
    {
        var bytes = File.ReadAllBytes(path);
        if (bytes.Length >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF)
            return Encoding.UTF8.GetString(bytes, 3, bytes.Length - 3);
        if (bytes.Length >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE)
            return Encoding.Unicode.GetString(bytes, 2, bytes.Length - 2);
        if (bytes.Length >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF)
            return Encoding.BigEndianUnicode.GetString(bytes, 2, bytes.Length - 2);
        var pairs = bytes.Length / 2;
        if (pairs >= 2)
        {
            int evenZero = 0, oddZero = 0;
            for (var i = 0; i < pairs; i++)
            {
                if (bytes[i * 2] == 0) evenZero++;
                if (bytes[i * 2 + 1] == 0) oddZero++;
            }
            if (oddZero * 2 >= pairs) return Encoding.Unicode.GetString(bytes, 0, pairs * 2);
            if (evenZero * 2 >= pairs) return Encoding.BigEndianUnicode.GetString(bytes, 0, pairs * 2);
        }
        return Encoding.UTF8.GetString(bytes);
    }

    private static string NormalizeAccounts(string text)
    {
        if (string.IsNullOrEmpty(text)) return text;
        if (text[0] == '\uFEFF') text = text.Substring(1);
        var lines = new List<string>();
        foreach (var rawLine in text.Replace("\r\n", "\n").Replace('\r', '\n').Split('\n'))
        {
            var line = rawLine.Trim();
            if (line.Length == 0) continue;
            var starts = new List<int>();
            foreach (Match match in MailboxPattern.Matches(line))
            {
                if (match.Index > 0)
                {
                    var previous = line[match.Index - 1];
                    if (previous != '-' && !char.IsWhiteSpace(previous)) continue;
                }
                starts.Add(match.Index);
            }
            if (starts.Count < 2)
            {
                lines.Add(line);
                continue;
            }
            for (var i = 0; i < starts.Count; i++)
            {
                var end = i + 1 < starts.Count ? starts[i + 1] : line.Length;
                var piece = line.Substring(starts[i], end - starts[i]).Trim();
                if (piece.Length > 0) lines.Add(piece);
            }
        }
        return string.Join("\r\n", lines);
    }

    private async Task RunBatchFile(string path)
    {
        loginCanceled = false;
        var child = Spawn("login-batch --file=\"" + path + "\"");
        login = child;
        message.Text = "批量登录进行中。Chrome 会为每个账号打开一个独立窗口。";
        UpdateButtons();
        try
        {
            var output = Task.Run(async () =>
            {
                string line;
                while ((line = await child.StandardOutput.ReadLineAsync()) != null)
                {
                    var text = line.Trim();
                    if (text.Length == 0 || IsDisposed || !IsHandleCreated) continue;
                    try { BeginInvoke(new Action(() => { if (!IsDisposed) message.Text = text; })); }
                    catch (InvalidOperationException) { }
                }
            });
            var error = child.StandardError.ReadToEndAsync();
            await Task.Run(() => child.WaitForExit());
            await Task.WhenAny(Task.WhenAll(output, error), Task.Delay(1500));
            if (loginCanceled) message.Text = "已取消批量登录。已完成的账号会保留。";
            else if (child.ExitCode == 0) message.Text = "批量登录完成，账号已加入列表。";
            else message.Text = "批量登录结束，有账号失败。详见 batch-login-report.json。";
            await RefreshAccounts();
        }
        finally
        {
            child.Dispose();
            login = null;
        }
    }

    private async Task ToggleAccount()
    {
        var account = Selected;
        await Manage((account.enabled ? "disable" : "enable") + " --id=" + AccountArg(account.id));
        await RefreshAccounts();
        message.Text = account.enabled ? "账号已停用。" : "账号已启用。";
    }

    private async Task RemoveAccount()
    {
        var account = Selected;
        if (MessageBox.Show(this, "删除账号「" + account.name + "」的本地代理会话？此操作保留 Chrome 配置。",
            "删除账号", MessageBoxButtons.YesNo, MessageBoxIcon.Warning, MessageBoxDefaultButton.Button2) != DialogResult.Yes)
            return;
        await Manage("remove --id=" + AccountArg(account.id) + (account.enabled ? "" : " --disabled"));
        await RefreshAccounts();
        message.Text = "本地代理会话已删除。";
    }

    private async Task StartProxy()
    {
        await RefreshAccounts();
        bool usable = false;
        foreach (ListViewItem row in accounts.Items)
        {
            var account = (Account)row.Tag;
            usable |= account.enabled && account.model != null;
        }
        if (!usable) throw new InvalidOperationException("请先登录并启用至少一个账号。");
        await StopPrevious();
        var child = Spawn("serve --desktop --port " + port.Value + " --balance " +
            (strategy.SelectedIndex == 0 ? "round-robin" : "least-used"), true);
        server = child;
        message.Text = "正在启动本地代理…";
        // The monitor owns the process until stdout and stderr have closed.
        MonitorProxy(child);
    }

    private async Task StopPrevious()
    {
        var script = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "stop-dragoman.ps1");
        if (!File.Exists(script)) return;
        var info = new ProcessStartInfo("powershell.exe",
            "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" + script + "\""
            + " -ExecutablePath \"" + executable + "\" -Port " + port.Value)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        using (var child = Process.Start(info))
        {
            if (child == null) return;
            var drain = child.StandardOutput.ReadToEndAsync();
            var error = child.StandardError.ReadToEndAsync();
            await Task.Run(() => child.WaitForExit(15000));
            await Task.WhenAll(drain, error);
        }
    }

    private async void MonitorProxy(Process child)
    {
        try
        {
            var stderr = child.StandardError.BaseStream.CopyToAsync(Stream.Null);
            string line;
            while ((line = await child.StandardOutput.ReadLineAsync()) != null)
            {
                if (line != "DESKTOP_READY" || closing || stopping) continue;
                ready = true;
                message.Text = "API 地址  " + Endpoint;
                UpdateButtons();
            }
            await stderr;
            await Task.Run(() => child.WaitForExit());
            if (!closing && !stopping)
                message.Text = child.ExitCode == 2 ? "端口已被占用，请换一个端口。" :
                    child.ExitCode == 0 ? "代理已停止。" : "代理已退出。请检查已启用的账号后重试。";
        }
        catch (Exception) { if (!closing) message.Text = "代理状态读取失败。"; }
        finally
        {
            TryKill(child);
            child.Dispose();
            server = null;
            ready = false;
            if (!IsDisposed) UpdateButtons();
        }
    }

    private async Task StopProxy()
    {
        var child = server;
        if (child == null) return;
        stopping = true;
        ready = false;
        message.Text = "正在停止代理…";
        UpdateButtons();
        try { child.StandardInput.Close(); }
        catch (IOException) { TryKill(child); }
        for (int i = 0; i < 50 && server == child; i++) await Task.Delay(100);
        if (server == child) TryKill(child);
        if (!closing) message.Text = server == null ? "代理已停止。" : "正在结束代理进程…";
        stopping = false;
    }

    private static void TryKill(Process child)
    {
        if (child == null) return;
        try { if (!child.HasExited) child.Kill(); }
        catch (InvalidOperationException) { }
        catch (System.ComponentModel.Win32Exception) { }
    }

    private async void OnClosing(object sender, FormClosingEventArgs e)
    {
        if (closing && server == null && !busy) return;
        e.Cancel = true;
        if (closing) return;
        closing = true;
        UpdateButtons();
        TryKill(login);
        while (busy) await Task.Delay(100);
        await StopProxy();
        while (server != null) await Task.Delay(100);
        Close();
    }
}
