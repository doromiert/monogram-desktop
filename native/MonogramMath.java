// SPDX-License-Identifier: GPL-3.0-or-later
// Desktop adapter for the JLaTeXMath engine used by Monogram mobile.
import java.awt.Color;
import java.awt.Graphics2D;
import java.awt.image.BufferedImage;
import java.nio.charset.StandardCharsets;
import javax.imageio.ImageIO;
import org.scilab.forge.jlatexmath.TeXConstants;
import org.scilab.forge.jlatexmath.TeXFormula;
import org.scilab.forge.jlatexmath.TeXIcon;

public final class MonogramMath {
    public static void main(String[] args) throws Exception {
        byte[] input=System.in.readNBytes(32769);
        String source=new String(input,StandardCharsets.UTF_8);
        if(input.length>32768 || source.length()>8192 || source.isEmpty() || args.length!=2) throw new IllegalArgumentException("Formula limit");
        // Formulas cannot import resources or alter parser-wide command definitions.
        for(String command:new String[]{"includegraphics","input","include","write","read","newcommand","renewcommand","newenvironment","def","catcode","special"})
            if(source.contains("\\"+command)) throw new IllegalArgumentException("Unsupported formula command");
        TeXIcon icon=new TeXFormula(source).createTeXIcon(args[0].equals("display") ? TeXConstants.STYLE_DISPLAY : TeXConstants.STYLE_TEXT,36);
        int width=icon.getIconWidth(),height=icon.getIconHeight();
        if(width<1 || height<1 || width>4096 || height>2048 || (long)width*height>4194304) throw new IllegalArgumentException("Formula image limit");
        icon.setForeground(new Color(Integer.parseUnsignedInt(args[1],16),true));
        BufferedImage image=new BufferedImage(width,height,BufferedImage.TYPE_INT_ARGB);
        Graphics2D graphics=image.createGraphics();icon.paintIcon(null,graphics,0,0);graphics.dispose();
        System.out.write((icon.getIconDepth()+"\n").getBytes(StandardCharsets.US_ASCII));
        if(!ImageIO.write(image,"png",System.out)) throw new IllegalStateException("PNG unavailable");
    }
}
