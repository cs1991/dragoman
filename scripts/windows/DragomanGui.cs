using System;
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
    }

    private readonly string executable = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "dragoman.exe");
    private readonly ListView accounts = new ListView();
    private readonly Label status = new Label { AutoSize = true };
    private readonly Label message = new Label { Dock = DockStyle.Fill, AutoEllipsis = true };
    private readonly NumericUpDown port = new NumericUpDown { Minimum = 1, Maximum = 65535, Value = 8080, Width = 78 };
    private readonly ComboBox strategy = new ComboBox { DropDownStyle = ComboBoxStyle.DropDownList, Width = 120 };
    private readonly Button start, stop, copy, add, loginAgain, toggle, remove, refresh, cancelLogin;
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
        ClientSize = new Size(810, 470);
        MinimumSize = new Size(760, 400);

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
        accounts.Columns.Add("账号", 275);
        accounts.Columns.Add("状态", 150);
        accounts.Columns.Add("模型", 190);
        accounts.SelectedIndexChanged += (s, e) => UpdateButtons();
        layout.Controls.Add(accounts, 0, 3);
        var accountBar = Bar(layout, 4);
        add = Button(accountBar, "新增账号", async () => await Run(AddAccount));
        loginAgain = Button(accountBar, "重新登录", async () => await Run(() => LogIn(Selected.id)));
        toggle = Button(accountBar, "停用", async () => await Run(ToggleAccount));
        remove = Button(accountBar, "删除", async () => await Run(RemoveAccount));
        refresh = Button(accountBar, "刷新列表", async () => await Run(RefreshAccounts));
        cancelLogin = Button(accountBar, "取消登录", () => { loginCanceled = true; TryKill(login); });
        layout.Controls.Add(message, 0, 5);
        Shown += async (s, e) => await Run(RefreshAccounts);
        FormClosing += OnClosing;
        UpdateButtons();
        message.Text = "账号更改前请停止代理。登录完成后可关闭对应的 Chrome 窗口。";
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
        add.Enabled = port.Enabled = strategy.Enabled = edit;
        remove.Enabled = toggle.Enabled = loginAgain.Enabled = edit && selected != null;
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
        await Task.WhenAll(output, error);
        await Task.Run(() => child.WaitForExit());
        if (child.ExitCode != 0) throw new InvalidOperationException(failure);
        return output.Result;
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
            accounts.Items.Add(row);
            row.Selected = previous != null && account.id == previous.id && account.enabled == previous.enabled;
        }
        accounts.EndUpdate();
    }

    private async Task AddAccount()
    {
        using (var dialog = new Form { Text = "新增账号", ClientSize = new Size(375, 135), Font = Font,
            StartPosition = FormStartPosition.CenterParent, FormBorderStyle = FormBorderStyle.FixedDialog,
            MinimizeBox = false, MaximizeBox = false, ShowInTaskbar = false })
        {
            var prompt = new Label { Text = "账号别名（字母、数字、点、横线或下划线）", AutoSize = true, Location = new Point(12, 12) };
            var name = new TextBox { Bounds = new Rectangle(12, 42, 350, 28), MaxLength = 64 };
            var ok = new Button { Text = "打开 Chrome", AutoSize = true, Location = new Point(12, 86) };
            ok.Click += (s, e) => {
                var value = name.Text.Trim();
                if (!Regex.IsMatch(value, @"\A[a-zA-Z0-9_-][a-zA-Z0-9._-]*\z"))
                    MessageBox.Show(dialog, "请输入有效的账号别名。", "Dragoman");
                else if (accounts.Items.Cast<ListViewItem>().Any(row =>
                    String.Equals(((Account)row.Tag).id, "named:" + value, StringComparison.OrdinalIgnoreCase)))
                    MessageBox.Show(dialog, "此别名已存在。请选择该账号并重新登录。", "Dragoman");
                else dialog.DialogResult = DialogResult.OK;
            };
            dialog.Controls.AddRange(new Control[] { prompt, name, ok });
            dialog.AcceptButton = ok;
            if (dialog.ShowDialog(this) == DialogResult.OK) await LogIn("named:" + name.Text.Trim());
        }
    }

    private async Task LogIn(string id)
    {
        loginCanceled = false;
        login = Spawn("login --desktop-id=" + AccountArg(id));
        message.Text = "请在 Chrome 中完成登录并打开 Gemini。已有同账号的 Chrome 窗口时，请关闭后重试。";
        UpdateButtons();
        try
        {
            await ReadResult(login, "登录未完成。请确认已安装 Chrome，关闭该账号的旧窗口后重试，并在 4 分钟内打开 Gemini。");
            message.Text = "会话已保存。现在可以关闭对应的 Chrome 窗口。";
            await RefreshAccounts();
        }
        catch (InvalidOperationException)
        {
            if (!loginCanceled) throw;
            message.Text = "已取消登录。可以关闭此次登录打开的 Chrome 窗口。";
        }
        finally { login.Dispose(); login = null; }
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
        var child = Spawn("serve --desktop --port " + port.Value + " --balance " +
            (strategy.SelectedIndex == 0 ? "round-robin" : "least-used"), true);
        server = child;
        message.Text = "正在启动本地代理…";
        // The monitor owns the process until stdout and stderr have closed.
        MonitorProxy(child);
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
